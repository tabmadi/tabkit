# ADR-0600: Binary cache

- **Status:** Accepted
- **Decides:** A repo with two variables uses a binary cache on niks3 and S3, CI pushes with OIDC, and other repos run as before.

## Context

A CI runner starts with an empty Nix store. The public cache of NixOS holds the packages of nixpkgs, but not the paths that a repo builds itself. In a measured run of a platform template, each job spent 430 to 550 seconds on its dev shell. Its own work took under ten seconds. The check job built 1,861 derivations from cold.

The paths that only these repos build are about 3.6 GB for a platform template and 3.3 GB for a smaller one.

The drivers:

1. **Removes the cost.** A runner downloads each path that an earlier job built, and builds nothing twice.
2. **Serves workstations too.** One cache serves CI and each engineer.
3. **No secret in a project or a fork.** Reads are public, and only the default branch of a listed GitHub account writes.
4. **Fits the size.** It holds several GB for each repo, with garbage collection.
5. **Cost.**
6. **Operator control.** The operator runs it, and a build still works without it.
7. **Operations.** What a person must run.

| Option | 1 Removes cost | 2 Workstations | 3 No secret | 4 Size | 5 Cost | 6 Control | 7 Operations | Verdict |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| cache-nix-action, the whole store in the Actions cache | Partly. Each job restores an archive of several GB | No | Yes | No. The free cache is 10 GB for each repo, and a cold store of a platform is 13.4 GB | Free to 10 GB | GitHub | None | Rejected on drivers 1 and 4 |
| Magic Nix Cache, single paths in the Actions cache | Yes | No | Yes | 10 GB and seven days, shared with every other cache | Free | GitHub and Determinate | None | Rejected on driver 2. Its own vendor names deep limitations |
| FlakeHub Cache | Yes | Yes, with a login | No. Each project sets up FlakeHub | Yes | Paid | Determinate | None | Rejected on drivers 3 and 5 |
| garnix | Yes | Yes | Yes | Yes | Free to 1,500 minutes a month | garnix | None | Rejected. It replaces the CI of GitHub, and the gate is `just ci` in a workflow |
| An S3 bucket with `nix copy` | Yes | Yes | No. CI needs write keys as a secret | No garbage collection | Small | Operator | Small | Rejected on drivers 3 and 4 |
| Cachix | Yes | Yes | A push token on the default branch only | No for every repo. The free plan holds 5 GB | Free to 5 GB | Cachix | None | The next candidate, if the operator runs no server |
| **niks3 on S3** | **Yes** | **Yes** | **Yes. CI pushes with the OIDC token of GitHub Actions** | **Yes, with garbage collection** | **One bucket and one small server** | **Operator** | **One service** | **Chosen** |

Sources:

- The Actions cache has 10 GB free for each repo, and more is paid: [GitHub changelog](https://github.blog/changelog/2025-11-20-github-actions-cache-size-can-now-exceed-10-gb-per-repository/).
- cache-nix-action restores and saves the Nix store: [cache-nix-action](https://github.com/nix-community/cache-nix-action).
- The Magic Nix Cache came back with limitations, and its vendor recommends FlakeHub Cache: [Determinate Systems](https://determinate.systems/blog/bringing-back-magic-nix-cache-action).
- Cachix gives 5 GB free to open source: [Cachix pricing](https://www.cachix.org/pricing).
- garnix includes 1,500 CI minutes a month for free: [garnix pricing](https://garnix.io/pricing/).
- niks3 stores paths in S3, collects garbage, and takes OIDC tokens from CI: [niks3](https://github.com/Mic92/niks3).
- ncps mirrors upstream caches and signs what it serves: [ncps](https://github.com/kalbasit/ncps).

## Decision

**The cache is `nixosModules.nix-cache`.** It imports the NixOS module of niks3, with PostgreSQL on the same machine, and SeaweedFS for the bucket by default.

| Option, under `tabkit.nixCache` | Sets |
| --- | --- |
| `domain` | The host name that clients read from and CI pushes to |
| `signingKeyFile` | The key that signs each path, at run time outside the Nix store |
| `githubOwners` and `branches` | The GitHub accounts and the refs whose workflows may push |
| `oidc` | Whether CI pushes with OIDC. niks3 then reads the OIDC discovery of GitHub at each start |
| `apiTokenFile` | A token for a push from outside CI, such as a workstation |
| `keepFor` | How long garbage collection keeps a closure |
| `s3.*` | The bucket, and `publicDomain`, the S3 host name for clients |
| `mirror.*` | ncps, a mirror of the public cache for the machines of one network |

**A client uploads to the bucket directly.** niks3 hands out presigned URLs, so the S3 endpoint has a public host name, by default `s3.<domain>`. nginx passes the Host header unchanged, because each URL signs it.

**The bucket stays private.** The read proxy of niks3 serves each read.

**The push is `nix-cache-push`, in the dev shell of every repo.** After `just ci` on the default branch, it pushes only the paths that the job built. A path from a cache is never pushed again. Without `NIX_CACHE_URL` or an OIDC token it does nothing. When the cache does not answer, it warns and exits with success, so a cache outage never fails a gate.

**A repo turns the cache on with two repository variables.** `NIX_CACHE_URL` and `NIX_CACHE_PUBLIC_KEY` add the cache as a substituter in CI. Without them, both settings are empty. A repo without the variables never trusts a cache by default.

**The nixosTest `nix-cache` proves each refusal.** A push without the token is refused. A client that trusts the key fetches a pushed path and builds nothing. A client that does not trust the key refuses the path. The mirror serves a path of its upstream, signed by its own key. The test uses input-addressed paths: a content-addressed path proves itself, so it passes without a signature.

## Consequences

The cache is one more machine to run. A build still works when it is down, because Nix falls back to the public cache and to a local build.

A cache host with OIDC needs the internet at each start. Without it, niks3 restarts until it reaches GitHub.

A workstation reads the cache only when its `nix.conf` lists the URL and the public key.

## Rules

- A repo turns the cache on with the variables `NIX_CACHE_URL` and `NIX_CACHE_PUBLIC_KEY`, never with a value in a committed file.
- Only the default branch of a listed GitHub account pushes, with the OIDC token of the job.
- `nix-cache-push` never fails a gate.
- A key or a token of the cache is a path at run time, outside the Nix store, and the module asserts it.
- The nixosTest `nix-cache` runs in CI. `(CI: ci)`
