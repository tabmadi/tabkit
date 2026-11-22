# ADR-0500: Self-hosted machines

- **Status:** Accepted
- **Decides:** A repo that runs its own machine takes the server base, the microVM host, and the deploy from tabnix, and a test proves each refusal.

## Context

A repo that runs one machine without an orchestrator needs the same parts each time. It needs a hardened server, and guests that each run one part of the app. It needs a firewall between the guests, a way to give each guest its secrets, and a deploy that cannot lock the operator out.

A pilot ran these parts on one dedicated server with a copy of the code in its own repo. It proved each part on the real machine. A deploy with a broken firewall rolled back by itself. A NixOS release upgrade passed only as a deploy for the next boot and a reboot, because a live switch failed. The pilot code names its own services, so no other repo can import it.

The drivers:

1. **Isolation.** A broken guest does not reach the data or the keys of another guest.
2. **Declared flows.** Each connection between guests is declared once, and the host drops every other one.
3. **Safe deploy.** A deploy that breaks SSH or the network rolls back with no action from a person.
4. **No secret in the store.** A secret value never enters the Nix store, which every user of the machine can read.
5. **Tested.** A test in CI boots the parts and asserts each refusal, per [ADR-0300](0300-nixos-modules.md).
6. **Small repo code.** A repo states its guests, flows, and nodes, not the code that makes them.

### The guests

| Option | 1 Isolation | 2 Declared flows | 5 Tested | Verdict |
| --- | --- | --- | --- | --- |
| Every service on the host | No. A broken service shares the kernel, the disk, and the users of every other one | Only per port, not per source service | Yes | Rejected |
| NixOS containers, on systemd-nspawn | Partly. They share the kernel, and the NixOS maintainers do not call them a security boundary | Yes, with a private network | Yes | Rejected. They fail driver 1 |
| Incus or libvirt VMs, with a separate guest config | Yes. Each guest has its own kernel | Yes | Partly. The guest is not one NixOS system of the flake | Rejected. A guest is a second thing to build and update |
| **microVMs with microvm.nix** | **Yes. Each guest has its own kernel under KVM, and only a read-only view of the store** | **Yes. One routed tap for each guest** | **Yes. The guests boot nested in a nixosTest** | **Chosen** |

A microVM costs more memory than a container, and its memory is fixed at start. A guest of a small app needs a few hundred MiB, so the cost is small.

### The secrets

| Option | 4 No secret in the store | 6 Small repo code | Verdict |
| --- | --- | --- | --- |
| The module runs sops-nix itself | Yes | Yes, for a repo on sops-nix. A repo on agenix or another tool cannot use it | Rejected. It ties every repo to one tool |
| A guest fetches its secrets from a vault | Yes | No. Each guest needs a vault client and a bootstrap credential | Rejected for the base. A repo can still add it |
| **The module takes a path at run time for each secret, and copies it into a read-only share of the guest** | **Yes. The module refuses a path in the store** | **Yes. Any tool that writes a file works** | **Chosen** |

### The deploy

| Option | 3 Safe deploy | 5 Tested | 6 Small repo code | Verdict |
| --- | --- | --- | --- | --- |
| `nixos-rebuild --target-host` | No. A broken firewall locks the operator out | Yes | Yes | Rejected |
| Colmena | No. Its docs name no rollback when the connection is lost | Yes | Yes. It suits a fleet | Rejected. Driver 3 fails |
| comin, which pulls from git on the machine | Partly. The operator reverts a commit, and the machine must still reach git | Yes | Yes | Rejected. A lockout needs the console |
| **deploy-rs** | **Yes. Magic rollback waits for a new SSH connection, and rolls back without one** | **Yes. Its checks validate each node, and its own nixosTests cover the deploy** | **Yes** | **Chosen** |

Sources:

- microvm.nix runs each guest under a hypervisor with its own kernel, and can share the store read-only: [microvm.nix README](https://github.com/microvm-nix/microvm.nix).
- NixOS containers are not a security boundary, and keep access to the Nix daemon: [NixOS Discourse, NixOS containers and security](https://discourse.nixos.org/t/nixos-containers-what-security-concern-are-there-when-switching-from-docker/79715).
- deploy-rs has magic rollback and checks for the deploy: [deploy-rs README](https://github.com/serokell/deploy-rs).
- A comparison of the deploy tools by their users: [NixOS Discourse, evaluating NixOps, deploy-rs, and nixos-rebuild](https://discourse.nixos.org/t/deployment-tools-evaluating-nixops-deploy-rs-and-vanilla-nix-rebuild/36388).
- Colmena deploys to hosts that run NixOS, over SSH: [Colmena manual](https://colmena.cli.rs/0.2/print.html).
- comin polls git and switches the machine: [NixOS Discourse, Comin](https://discourse.nixos.org/t/comin-git-push-nixos-machines/40688).

## Decision

**The server base is `nixosModules.server`.** It sets SSH with keys only, the nftables firewall, and Nix with flakes and a weekly garbage collection.

| Option, under `tabnix.server` | Sets |
| --- | --- |
| `enable` | Whether the machine takes the base |
| `adminKeys` | The SSH public keys that log in as root |
| `keepGenerations` | How long the garbage collection keeps an old generation, so a rollback can reach it |

**The microVM host is `nixosModules.microvm-host`.** Each guest is a microVM with one tap. The host holds the gateway address on each tap and routes each guest address to its tap, so no bridge joins two guests. Every packet between guests crosses the forward chain of the host.

| Option, under `tabnix.microvmHost` | Sets |
| --- | --- |
| `enable` | Whether the machine runs the guests |
| `gateway` | The address of the host on each tap |
| `uplink` | The interface that leads beyond the host |
| `nameservers` | The DNS resolvers of every guest |
| `adminKeys` | The SSH keys that log in to each guest as root, from the host only |
| `guests.<name>` | The address, MAC, CPUs, memory, data volume, egress, published ports, secrets, and NixOS config of one guest |
| `flows` | The connections that one guest may open to another, by ports |

The forward chain accepts only three kinds of traffic. These are the declared flows, the published ports after their destination NAT, and the traffic of a guest with `egress` to the uplink. The input firewall of each guest opens each port of a flow to the source of that flow only. A published port forwards from every address of the uplink, as `networking.nat.forwardPorts` does.

Each guest reads its secrets at `/run/host-secrets`, a read-only share. A unit on the host copies each secret from its path into the share before the guest starts. The share is in `/run`, so no secret reaches a disk. The store and the secrets are shared with virtiofs, and `/var` of each guest is a volume.

**The deploy is `flakeModules.deploy`.** It is an opt-in flake module of [ADR-0400](0400-opt-in-flake-modules.md), because a deploy is a flake output, not a NixOS option. Each node deploys one `nixosConfiguration` with deploy-rs, with magic rollback and auto rollback on. The checks of deploy-rs become checks of the repo.

| Option, under `tabnix.deploy.nodes.<name>` | Sets |
| --- | --- |
| `hostname` | The address that the deploy connects to |
| `configuration` | The `nixosConfiguration` that the node runs, by default the name of the node |
| `sshUser` and `sshOpts` | How the deploy logs in |
| `activationTimeout` | The time for an activation, by default 900 seconds, so a restart of every guest fits |
| `confirmTimeout` | The time that the node waits for a new SSH connection before it rolls back, by default 300 seconds |

**The first deploy after an install changes nothing.** A rollback of deploy-rs reactivates the previous generation with the activation script of deploy-rs. A generation that the installer made has no such script, so a rollback to it fails and the machine stays on the broken config. A QEMU copy of a host proved this.

**A release upgrade of NixOS is a deploy for the next boot, then a reboot.** A live switch across releases can fail, as the pilot found. Run `deploy --boot`, then reboot the machine.

**Each module has a test.** The nixosTest `server` proves that a password never logs in and that a closed port stays closed. The nixosTest `microvm-host` boots two nested guests. It proves that a port that no flow names is closed, and that a flow opens one direction only. It proves that only a guest with egress reaches beyond the host, and only a published port reaches a guest from beyond the host. It proves that a guest cannot change its secrets or read the secrets of another guest. The check `module-deploy` builds a fixture node, asserts the rollback settings, and runs the checks of deploy-rs.

## Consequences

**The `microvm-host` test is not a CI check.** Its guests run inside the test VM. A GitHub runner is a VM itself, so there a guest is three levels deep, and it hangs at boot. The test is the package `nested-microvm-host`, and `just test-nested` builds it on a Linux machine with KVM. `just ci` runs `just test-nested`, so the pre-push hook runs it before each push, and GitHub CI skips it.

CI still evaluates and builds the module, and a repo that uses it builds its host in its own CI. What only the nested test proves is the behaviour: the flows, the egress, the published ports, and the secrets. A Renovate update merges with CI alone, so a release of tabnix needs a run of `just ci` on a Linux machine with KVM first.

A repo takes the versions of microvm.nix and deploy-rs that tabnix pins.

A guest has no Nix of its own. The host builds every guest, and a deploy of the host restarts each changed guest.

The installer of the host and the layout of its disks stay in the repo, because they name its hardware.

## Rules

- A machine that a repo runs imports `nixosModules.server`.
- A guest is a `tabnix.microvmHost.guests` entry, and a connection between guests is a `tabnix.microvmHost.flows` entry. No rule of the repo opens the forward chain by hand.
- A secret of a guest is a path at run time outside the Nix store, and the module asserts it.
- A repo deploys a machine through `flakeModules.deploy`, with magic rollback on. `(CI: ci)`
- The first deploy after an install deploys the installed config again, so that a rollback has a generation of deploy-rs to return to.
- A release upgrade of NixOS is `deploy --boot` and a reboot, never a live switch.
- The nixosTest `server` and the check `module-deploy` run in CI. `(CI: ci)`
- The nested test `microvm-host` is not a CI check. `just ci` runs it on Linux with KVM, so the pre-push hook runs it.
- A tag of tabnix follows a pass of `just ci` on a Linux machine with KVM.
