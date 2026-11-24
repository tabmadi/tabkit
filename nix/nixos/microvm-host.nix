# The microVM host, per ADR-0500. The host runs each guest as a microVM with one routed tap. No bridge joins two
# guests, so every packet between guests crosses the forward chain of the host, and only the declared flows pass.
localInputs:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib) mkOption types;

  cfg = config.tabkit.microvmHost;
  names = builtins.attrNames cfg.guests;
  tap = name: "vm-${name}";
  address = name: cfg.guests.${name}.address;
  egressGuests = lib.filter (n: cfg.guests.${n}.egress) names;
  shareDir = name: "/run/tabkit-guests/${name}";

  ports = list: "{ ${lib.concatMapStringsSep ", " toString list} }";

  # An empty list of sources accepts a published port from any address.
  sourceMatch =
    sources: lib.optionalString (sources != [ ]) "ip saddr { ${lib.concatStringsSep ", " sources} } ";

  # One rule for each declared flow. The forward chain drops every other new connection between guests.
  flowRule =
    flow:
    "ip saddr ${address flow.from} ip daddr ${address flow.to} ${flow.protocol} dport ${ports flow.ports} accept";

  # A published port reaches its guest only through the destination NAT of the host.
  publishRules = lib.concatMap (
    name:
    map (
      port:
      "${
        sourceMatch cfg.guests.${name}.publishSources
      }ip daddr ${address name} tcp dport ${toString port} ct status dnat accept"
    ) cfg.guests.${name}.publish
  ) names;

  egressRule = lib.optionalString (cfg.uplink != null && egressGuests != [ ]) ''
    iifname { ${
      lib.concatMapStringsSep ", " (n: ''"${tap n}"'') egressGuests
    } } oifname "${cfg.uplink}" accept
  '';

  flowOptions = {
    options = {
      from = mkOption {
        type = types.str;
        example = "app";
        description = "The name of the guest that opens the connection.";
      };
      to = mkOption {
        type = types.str;
        example = "db";
        description = "The name of the guest that accepts the connection.";
      };
      ports = mkOption {
        type = types.nonEmptyListOf types.port;
        example = [ 5432 ];
        description = "The destination ports that the flow opens.";
      };
      protocol = mkOption {
        type = types.enum [
          "tcp"
          "udp"
        ];
        default = "tcp";
        description = "The transport protocol of the flow.";
      };
    };
  };

  guestOptions =
    { name, ... }:
    {
      options = {
        address = mkOption {
          type = types.str;
          example = "10.20.0.10";
          description = "The IPv4 address of the guest. The host routes it to the tap of the guest.";
        };
        mac = mkOption {
          type = types.strMatching "02:([0-9a-f]{2}:){4}[0-9a-f]{2}";
          example = "02:00:00:00:00:10";
          description = "The MAC address of the guest. It is a locally administered address.";
        };
        vcpu = mkOption {
          type = types.ints.positive;
          default = 1;
          description = "The number of virtual CPUs of the guest.";
        };
        memory = mkOption {
          type = types.ints.positive;
          default = 512;
          description = "The memory of the guest, in MiB.";
        };
        dataSize = mkOption {
          type = types.ints.positive;
          default = 1024;
          description = "The size of the data volume that holds /var of the guest, in MiB.";
        };
        egress = mkOption {
          type = types.bool;
          default = false;
          description = "Whether the guest reaches the network beyond the host, through the NAT of the uplink.";
        };
        publish = mkOption {
          type = types.listOf types.port;
          default = [ ];
          example = [ 443 ];
          description = "The TCP ports of the uplink that the host forwards to the same port of the guest.";
        };
        publishSources = mkOption {
          type = types.listOf types.str;
          default = [ ];
          example = [ "203.0.113.0/24" ];
          description = "The IPv4 source ranges that may reach the published ports. An empty list accepts any source.";
        };
        secretsRevision = mkOption {
          type = types.str;
          default = "";
          example = "a hash of the encrypted secrets file";
          description = "A value that changes with the secret values. A new value copies the secrets again and restarts the guest.";
        };
        secrets = mkOption {
          type = types.attrsOf types.str;
          default = { };
          example = {
            "app.env" = "/run/secrets/app.env";
          };
          description = "The files that the guest reads at /run/host-secrets, by name. Each value is a path on the host at run time, outside the Nix store.";
        };
        config = mkOption {
          type = types.deferredModule;
          default = { };
          description = "The NixOS configuration of the guest, beside the base that this module sets.";
        };
      };
    };

  # The base of every guest: one routed tap, the store and the secrets as read-only shares, /var on a volume, and an
  # input firewall that opens SSH to the host and each port of a flow to the source of that flow only.
  guestBase =
    name:
    let
      guest = cfg.guests.${name};
      inbound = lib.filter (f: f.to == name) cfg.flows;
    in
    {
      system.stateVersion = lib.mkDefault config.system.stateVersion;
      networking.hostName = tap name;

      microvm = {
        hypervisor = "qemu";
        inherit (guest) vcpu;
        mem = guest.memory;
        interfaces = [
          {
            type = "tap";
            id = tap name;
            inherit (guest) mac;
            tap.vhost = true;
          }
        ];
        shares = [
          {
            proto = "virtiofs";
            tag = "ro-store";
            source = "/nix/store";
            mountPoint = "/nix/.ro-store";
          }
          {
            proto = "virtiofs";
            tag = "secrets";
            source = "${shareDir name}/secrets";
            mountPoint = "/run/host-secrets";
            readOnly = true;
          }
        ];
        volumes = [
          {
            image = "var.img";
            mountPoint = "/var";
            size = guest.dataSize;
          }
        ];
      };

      networking.useDHCP = false;
      networking.useNetworkd = true;
      networking.nameservers = cfg.nameservers;
      systemd.network.enable = true;
      systemd.network.networks."10-host" = {
        matchConfig.MACAddress = guest.mac;
        address = [ "${guest.address}/32" ];
        routes = [
          {
            Gateway = cfg.gateway;
            GatewayOnLink = true;
          }
        ];
      };

      networking.nftables.enable = true;
      networking.firewall.enable = true;
      networking.firewall.extraInputRules = lib.concatStringsSep "\n" (
        [ "ip saddr ${cfg.gateway} tcp dport 22 accept" ]
        ++ map (f: "ip saddr ${address f.from} ${f.protocol} dport ${ports f.ports} accept") inbound
        # The hairpin NAT of the host rewrites the source of a guest that calls a published port to the gateway.
        # The forward chain has already checked that connection against the flows.
        ++
          lib.optional (guest.publish != [ ])
            "${
              sourceMatch (lib.optionals (guest.publishSources != [ ]) (guest.publishSources ++ [ cfg.gateway ]))
            }tcp dport ${ports guest.publish} accept"
      );

      services.timesyncd.servers = [ cfg.gateway ];

      services.openssh = {
        enable = true;
        openFirewall = false;
        hostKeys = [
          {
            path = "/var/lib/ssh/ssh_host_ed25519_key";
            type = "ed25519";
          }
        ];
        settings = {
          PasswordAuthentication = false;
          KbdInteractiveAuthentication = false;
          PermitRootLogin = "prohibit-password";
        };
      };
      users.users.root.openssh.authorizedKeys.keys = cfg.adminKeys;
      systemd.tmpfiles.rules = [ "d /var/lib/ssh 0700 root root -" ];

      # The host builds and deploys each guest, so a guest has no Nix of its own.
      nix.enable = false;
      documentation.enable = false;
    };
in
{
  imports = [ localInputs.microvm.nixosModules.host ];

  options.tabkit.microvmHost = {
    enable = mkOption {
      type = types.bool;
      default = false;
      description = "Whether the machine runs the guests of this option as microVMs.";
    };
    gateway = mkOption {
      type = types.str;
      example = "10.20.0.1";
      description = "The address that the host holds on each tap. Each guest routes through it.";
    };
    uplink = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "uplink";
      description = "The interface of the host that leads beyond it. A guest with egress or a published port needs it.";
    };
    nameservers = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "The DNS resolvers of every guest.";
    };
    adminKeys = mkOption {
      type = types.listOf types.str;
      default = [ ];
      description = "The SSH public keys that log in to each guest as root, from the host only.";
    };
    guests = mkOption {
      type = types.attrsOf (types.submodule guestOptions);
      default = { };
      description = "The guests of the host, by name. A name has at most 12 characters, so its tap name fits.";
    };
    flows = mkOption {
      type = types.listOf (types.submodule flowOptions);
      default = [ ];
      description = "The connections that one guest may open to another. The host drops every other one.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = lib.all (n: builtins.stringLength n <= 12) names;
        message = "tabkit.microvmHost: a guest name has at most 12 characters, so that its tap name fits in 15.";
      }
      {
        assertion = lib.all (f: cfg.guests ? ${f.from} && cfg.guests ? ${f.to}) cfg.flows;
        message = "tabkit.microvmHost.flows names a guest that is not in tabkit.microvmHost.guests.";
      }
      {
        assertion =
          cfg.uplink != null || lib.all (n: !cfg.guests.${n}.egress && cfg.guests.${n}.publish == [ ]) names;
        message = "tabkit.microvmHost: a guest with egress or a published port needs tabkit.microvmHost.uplink.";
      }
      {
        assertion = lib.all (
          n: lib.all (p: !lib.hasPrefix builtins.storeDir p) (builtins.attrValues cfg.guests.${n}.secrets)
        ) names;
        message = "tabkit.microvmHost: a secret is a path at run time, outside the Nix store.";
      }
      {
        assertion = config.networking.nftables.enable;
        message = "tabkit.microvmHost needs networking.nftables.enable, because it adds nftables rules.";
      }
    ];

    # Each tap holds the gateway, with a host route to its one guest. The host answers ARP for every guest.
    networking.useNetworkd = lib.mkDefault true;
    systemd.network.enable = true;
    systemd.network.networks = lib.listToAttrs (
      map (
        name:
        lib.nameValuePair "20-${tap name}" {
          matchConfig.Name = tap name;
          address = [ "${cfg.gateway}/32" ];
          routes = [
            {
              Destination = "${address name}/32";
              Scope = "link";
            }
          ];
          networkConfig.ConfigureWithoutCarrier = true;
          linkConfig.RequiredForOnline = "no";
        }
      ) names
    );

    boot.kernel.sysctl = {
      "net.ipv4.ip_forward" = 1;
      "net.ipv4.conf.all.proxy_arp" = 1;
    };
    boot.kernelModules = [ "vhost_net" ];

    networking.nat = lib.mkIf (cfg.uplink != null) {
      enable = true;
      externalInterface = cfg.uplink;
      internalIPs = map (n: "${address n}/32") egressGuests;
      forwardPorts = lib.concatMap (
        name:
        map (port: {
          sourcePort = port;
          destination = "${address name}:${toString port}";
          proto = "tcp";
        }) cfg.guests.${name}.publish
      ) names;
    };

    networking.firewall = {
      filterForward = true;
      extraForwardRules = lib.concatStringsSep "\n" (
        map flowRule cfg.flows ++ publishRules ++ [ egressRule ]
      );
      # A guest reaches the host only for time.
      interfaces = lib.genAttrs (map tap names) (_: {
        allowedUDPPorts = [ 123 ];
      });
    };

    services.chrony = {
      enable = true;
      extraConfig = lib.concatMapStrings (n: "allow ${address n}\n") names;
    };

    microvm.vms = lib.genAttrs names (name: {
      config = {
        imports = [
          (guestBase name)
          cfg.guests.${name}.config
        ];
      };
    });
    microvm.autostart = names;

    # A guest cannot follow a link into the host, so this unit copies each secret into the share of its guest
    # before the guest starts. The share lives in /run, so no secret reaches a disk.
    systemd.services = lib.listToAttrs (
      map (
        name:
        lib.nameValuePair "tabkit-guest-secrets-${name}" {
          description = "Copy the secrets of guest ${name} into its share";
          before = [ "microvm@${name}.service" ];
          requiredBy = [ "microvm@${name}.service" ];
          restartTriggers = [
            (builtins.toJSON cfg.guests.${name}.secrets)
            cfg.guests.${name}.secretsRevision
          ];
          serviceConfig = {
            Type = "oneshot";
            RemainAfterExit = true;
          };
          script = ''
            dir=${shareDir name}/secrets
            rm -rf "$dir"
            install -d -m 0700 "$dir"
            ${lib.concatStrings (
              lib.mapAttrsToList (file: source: ''
                install -m 0400 ${lib.escapeShellArg source} "$dir"/${lib.escapeShellArg file}
              '') cfg.guests.${name}.secrets
            )}
          '';
        }
      ) names
    );
  };
}
