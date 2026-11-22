# A binary cache for the repos that set its variables, per ADR-0600. niks3 stores the paths that CI builds in an S3 bucket, and CI
# pushes with the OIDC token of GitHub Actions, so no secret lives in a workflow. ncps mirrors the public cache.
localInputs:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib) mkOption types;

  cfg = config.tabkit.nixCache;

  inStore = path: path != null && lib.hasPrefix builtins.storeDir path;

  # The one S3 identity of the local SeaweedFS. systemd hands the two key files to the unit as credentials.
  seaweedConfig = pkgs.writeShellScript "seaweedfs-s3-config" ''
    umask 077
    ${lib.getExe pkgs.jq} -n \
      --rawfile access "$CREDENTIALS_DIRECTORY/access" --rawfile secret "$CREDENTIALS_DIRECTORY/secret" \
      '{identities: [{name: "niks3", credentials: [{accessKey: ($access | rtrimstr("\n")), secretKey: ($secret | rtrimstr("\n"))}], actions: ["Admin", "Read", "Write", "List", "Tagging"]}]}' \
      > "$RUNTIME_DIRECTORY/s3.json"
  '';
in
{
  imports = [ localInputs.niks3.nixosModules.niks3 ];

  options.tabkit.nixCache = {
    enable = mkOption {
      type = types.bool;
      default = false;
      description = "Whether the machine serves the binary cache.";
    };
    domain = mkOption {
      type = types.str;
      example = "cache.example.com";
      description = "The public host name of the cache. Clients read from it, and CI pushes to it.";
    };
    acme = mkOption {
      type = types.bool;
      default = true;
      description = "Whether nginx gets a certificate from Let's Encrypt for each host name. A test turns it off.";
    };
    signingKeyFile = mkOption {
      type = types.str;
      example = "/run/secrets/nix-cache.key";
      description = "The path at run time of the key that signs each path, made by `nix key generate-secret`. It is outside the Nix store.";
    };
    oidc = mkOption {
      type = types.bool;
      default = true;
      description = "Whether CI pushes with the OIDC token of GitHub Actions. niks3 then reads the OIDC discovery of GitHub at each start, so the host needs the internet to start.";
    };
    githubOwners = mkOption {
      type = types.nonEmptyListOf types.str;
      example = [ "tabmadi" ];
      description = "The GitHub accounts whose workflows may push, through the OIDC token of GitHub Actions.";
    };
    branches = mkOption {
      type = types.listOf types.str;
      default = [
        "refs/heads/master"
        "refs/heads/main"
      ];
      description = "The git refs whose workflows may push. A pull request never pushes.";
    };
    apiTokenFile = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = "The path at run time of a token for a push from outside CI, such as a workstation. Null allows no such push.";
    };
    keepFor = mkOption {
      type = types.str;
      default = "720h";
      description = "How long the garbage collection keeps a pushed closure, as a Go duration.";
    };
    s3 = {
      local = mkOption {
        type = types.bool;
        default = true;
        description = "Whether a SeaweedFS on the same machine holds the bucket, per ADR-0207 of a platform. False uses the endpoint below.";
      };
      publicDomain = mkOption {
        type = types.str;
        default = "s3.${cfg.domain}";
        defaultText = lib.literalExpression ''"s3.''${config.tabkit.nixCache.domain}"'';
        description = "The host name of the S3 endpoint for clients. niks3 hands each client a presigned URL, and the client uploads to the bucket directly.";
      };
      endpoint = mkOption {
        type = types.str;
        default = "127.0.0.1:8333";
        description = "The S3 endpoint, as host and port.";
      };
      useSSL = mkOption {
        type = types.bool;
        default = false;
        description = "Whether the connection to the S3 endpoint uses TLS.";
      };
      bucket = mkOption {
        type = types.str;
        default = "nix-cache";
        description = "The bucket that holds the paths.";
      };
      region = mkOption {
        type = types.str;
        default = "us-east-1";
        description = "The S3 region.";
      };
      accessKeyFile = mkOption {
        type = types.str;
        description = "The path at run time of the S3 access key.";
      };
      secretKeyFile = mkOption {
        type = types.str;
        description = "The path at run time of the S3 secret key.";
      };
    };
    mirror = {
      enable = mkOption {
        type = types.bool;
        default = true;
        description = "Whether ncps mirrors the upstream caches, so each machine of the network downloads a path once.";
      };
      domain = mkOption {
        type = types.str;
        example = "mirror.cache.example.com";
        description = "The host name of the mirror.";
      };
      signingKeyFile = mkOption {
        type = types.str;
        description = "The path at run time of the key that signs the paths of the mirror.";
      };
      upstreams = mkOption {
        type = types.listOf types.str;
        default = [ "https://cache.nixos.org" ];
        description = "The caches that the mirror fetches from.";
      };
      upstreamKeys = mkOption {
        type = types.listOf types.str;
        default = [ "cache.nixos.org-1:6NCHdD59X431o0gWypbMrAURkbJ16ZPMQFGspcDShjY=" ];
        description = "The public keys of the upstream caches. The mirror refuses a path without a valid signature.";
      };
      maxSize = mkOption {
        type = types.str;
        default = "50G";
        description = "The disk that the mirror may use. It evicts the least recently used paths beyond it.";
      };
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        assertions = [
          {
            assertion =
              !lib.any inStore [
                cfg.signingKeyFile
                cfg.apiTokenFile
                cfg.s3.accessKeyFile
                cfg.s3.secretKeyFile
                (if cfg.mirror.enable then cfg.mirror.signingKeyFile else null)
              ];
            message = "tabkit.nixCache: a key or a token is a path at run time, outside the Nix store.";
          }
        ];

        services.niks3 = {
          enable = true;
          # The build of nixpkgs, which the public cache holds. The module of niks3 builds its own from source.
          package = pkgs.niks3;
          serverPackage = pkgs.niks3;
          httpAddr = "127.0.0.1:5751";
          database.createLocally = true;
          s3 = {
            inherit (cfg.s3)
              endpoint
              bucket
              region
              useSSL
              accessKeyFile
              secretKeyFile
              ;
            publicUrl = "${if cfg.acme then "https" else "http"}://${cfg.s3.publicDomain}";
          };
          inherit (cfg) apiTokenFile;
          oidc.providers = lib.mkIf cfg.oidc {
            github = {
              issuer = "https://token.actions.githubusercontent.com";
              audience = "https://${cfg.domain}";
              boundClaims = {
                repository_owner = cfg.githubOwners;
                ref = cfg.branches;
              };
            };
          };
          signKeyFiles = [ cfg.signingKeyFile ];
          cacheUrl = "https://${cfg.domain}";
          # The bucket stays private. niks3 serves each read, so the bucket needs no public policy.
          readProxy.enable = true;
          nginx = {
            enable = true;
            inherit (cfg) domain;
            enableACME = cfg.acme;
            forceSSL = cfg.acme;
          };
          gc = {
            enable = true;
            olderThan = cfg.keepFor;
          };
        };

        networking.firewall.allowedTCPPorts = [
          80
          443
        ];
      }

      (lib.mkIf cfg.s3.local {
        # One process holds the master, the volume, the filer, and the S3 gateway, on the loopback address only.
        systemd.services.seaweedfs = {
          description = "SeaweedFS, the S3 store of the binary cache";
          wantedBy = [ "multi-user.target" ];
          before = [ "niks3.service" ];
          requiredBy = [ "niks3.service" ];
          serviceConfig = {
            DynamicUser = true;
            StateDirectory = "seaweedfs";
            RuntimeDirectory = "seaweedfs";
            LoadCredential = [
              "access:${cfg.s3.accessKeyFile}"
              "secret:${cfg.s3.secretKeyFile}"
            ];
            ExecStartPre = seaweedConfig;
            ExecStart = lib.concatStringsSep " " [
              "${pkgs.seaweedfs}/bin/weed mini"
              "-dir=/var/lib/seaweedfs"
              "-ip=127.0.0.1"
              "-ip.bind=127.0.0.1"
              "-s3.port=8333"
              "-s3.config=/run/seaweedfs/s3.json"
              "-bucket=${cfg.s3.bucket}"
            ];
            Restart = "on-failure";
          };
        };
        # The S3 endpoint for clients. The Host header passes unchanged, because each presigned URL signs it.
        services.nginx.virtualHosts.${cfg.s3.publicDomain} = {
          enableACME = cfg.acme;
          forceSSL = cfg.acme;
          locations."/" = {
            proxyPass = "http://127.0.0.1:8333";
            extraConfig = ''
              client_max_body_size 0;
              proxy_request_buffering off;
              proxy_set_header Host $host;
            '';
          };
        };
      })

      (lib.mkIf cfg.mirror.enable {
        services.ncps = {
          enable = true;
          cache = {
            hostName = cfg.mirror.domain;
            secretKeyPath = cfg.mirror.signingKeyFile;
            maxSize = cfg.mirror.maxSize;
            upstream = {
              urls = cfg.mirror.upstreams;
              publicKeys = cfg.mirror.upstreamKeys;
            };
          };
          server.addr = "127.0.0.1:8501";
        };
        services.nginx.virtualHosts.${cfg.mirror.domain} = {
          enableACME = cfg.acme;
          forceSSL = cfg.acme;
          locations."/".proxyPass = "http://127.0.0.1:8501";
        };
      })
    ]
  );
}
