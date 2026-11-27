# ADR-1300: Network egress

- **Status:** Accepted
- **Decides:** Machines read internal mirrors at four egress levels, and one file per machine holds the egress settings.

## Context

A developer machine or a server can have direct internet, only an HTTP proxy, only internal services, or no network outside its site. Each case needs a working build, a working dev shell, and a working deploy.

Each tool that fetches has its own proxy settings: the Nix daemon, Docker, BuildKit, kind, Argo CD, and the package managers. Settings that each tool repeats drift. A cache miss that goes to the internet without notice breaks a site that has no internet.

The drivers:

1. **One client setup.** A client has the same settings at every level.
2. **No new tool.** A level uses the tools that a repo already runs: Nix, zot, Forgejo, oras, and git.
3. **A proof.** A test proves the proxy level and the level with no network.

| Option | Verdict |
| --- | --- |
| Proxy settings on each client | Rejected on driver 1. Every puller needs the same settings, and each copy can be wrong |
| Zarf for the air gap | Rejected on driver 2. It adds its own registry, its own Git server, and a webhook that rewrites image references. These duplicate zot and Forgejo, and they conflict with signature checks |
| Hauler for the air gap | Rejected on driver 2. It saves images, charts, and files to one archive. zot data, the Nix store, and a git bundle do the same with about 50 lines of code |
| ko for Go images | Rejected on driver 2. It is as fast as a Docker build, BuildKit stays for the frontend, and it cannot put dbmate into a service image |
| Images built by Nix | Rejected. A rebuild after an edit took 15 s, against 1.6 s for Docker, because Nix keeps no Go build cache |
| **Internal mirrors at four levels** | **Chosen** |

## Decision

**Four levels.** Each level adds one step to the level before it.

| Level | Name | Clients read from | The mirrors fill from |
| --- | --- | --- | --- |
| L0 | `direct` | the mirrors, or the upstreams when a machine has no mirror | the upstreams, directly |
| L1 | `proxy` | the same mirrors | the upstreams, through the proxy |
| L2 | `internal` | the same mirrors | an import job on a network with access |
| L3 | `airgap` | the same mirrors | a bundle that a person carries in |

**The mirrors.** The Nix cache of [ADR-0600](0600-binary-cache.md) serves Nix paths. Every language package and every Helm chart is a fixed-output path, so the Nix cache serves them too. zot serves images. Forgejo serves Git.

**One file per machine.** `/etc/tabkit/egress.env` holds the settings as shell assignments.

| Key | Holds |
| --- | --- |
| `TABKIT_EGRESS` | The level: `direct`, `proxy`, `internal`, or `airgap` |
| `HTTP_PROXY`, `HTTPS_PROXY`, and the lowercase forms | The proxy URL, at the `proxy` level only |
| `NO_PROXY` and `no_proxy` | The bypass list of the site |

The file is absent on a machine with direct internet. The dev shell loads it. The Nix daemon and Docker load it through a systemd drop-in named `tabkit-egress.conf`.

**One bypass list.** tabkit-ops adds a fixed base to the site list: loopback, the private ranges, the cluster domains, and `.localtest.me`. Every command that sets `NO_PROXY` takes the list from that one function.

**A NixOS machine uses `nixosModules.egress`.** It writes the file, adds the drop-in to each unit that fetches, and installs the site CA. At the `internal` and `airgap` levels, the internal caches are the only Nix substituters. So a cache miss fails and never reaches the internet.

**Another machine uses `tabkit-ops egress setup`.** It checks the daemons and the Docker CLI config, and it writes the files that need root for a person to install.

**A bundle carries the mirrors across an air gap.** `tabkit-ops bundle export` writes the Nix closure as a signed file cache, each image as an OCI layout, a git bundle, and a manifest of the pins. `tabkit-ops bundle import` loads each part into the Nix store, the registry, and the forge.

**Some services need the internet for their purpose.** They run only on the side with access, or a site replaces them.

| Service | At `internal` and `airgap` |
| --- | --- |
| Dependency updates, such as Renovate | Runs on the side with access. Its merged pins go into the next bundle |
| Vulnerability databases | An imported snapshot, refreshed with each bundle |
| Public ACME certificates | The site CA |
| The OIDC discovery of a forge | A token in place of OIDC |
| Public DNS and upstream checks | Run on the side with access only |

**A service has one build file.** It lives in the directory of the service, and the repo finds it by the directory name. A repo has one flake and one lock.

## Consequences

A machine at `internal` or `airgap` gets a new pin only through the next import. A Renovate update waits for the next bundle.

The level with no network fails on any fetch that the bundle does not hold. The failure names the missing path or image.

## Rules

- A machine holds its egress settings in `/etc/tabkit/egress.env`, and no repo file holds a proxy URL.
- Every command that sets `NO_PROXY` takes the list from the bypass function of tabkit-ops.
- The Nix daemon and Docker load the egress file through the drop-in `tabkit-egress.conf`.
- At the `internal` and `airgap` levels, the internal caches are the only Nix substituters.
- A new tool for egress needs a new decision, and the options table above records the tools that lost.
- The nixosTest `egress` proves the `proxy` level and the `airgap` level. `(CI: ci)`
