# ADR-0300: NixOS modules

- **Status:** Accepted
- **Decides:** tabnix ships opt-in NixOS modules for the machines that a repo runs, and a nixosTest in CI proves each refusal of each module.

## Context

Some repos run their own machines on NixOS. Each such repo repeats the same machine code: the gates between services, the firewall rules, and the paths of the secrets. Copied code drifts, the same as the toolchain of [ADR-0000](0000-foundations.md).

A module that guards a trust boundary must prove that it refuses what it must refuse. A review of the code is not that proof. Only a running machine is.

| Option | Verdict |
| --- | --- |
| A copy in each repo | Each copy drifts, and each repo must write its own test |
| An option of the flake-parts module | Every repo loads it, and most repos run no machine |
| **A `nixosModules` output of the flake** | **Chosen. A repo imports a module only on a machine that it runs, and the module has its test beside it** |

## Decision

**A NixOS module is a separate output.** It is `nixosModules.<name>` of the tabnix flake, from the file `nixos/<name>.nix`. The flake-parts module of [ADR-0200](0200-module.md) never imports it, so a repo without machines never loads it.

**Its options are under `tabnix.`** in the option tree of NixOS. So a name never clashes with an option of nixpkgs.

**Every module has a nixosTest.** The test is `tests/<name>.nix`, and it is a check of the own flake of tabnix on Linux. It boots VMs, and it asserts each refusal of the module, not only the path that succeeds.

**CI runs every test.** The workflow gives the runner KVM, and `just ci` runs the tests with the other checks. Nix caches a test whose inputs did not change, so a local run repeats only a changed test.

**A secret never enters the Nix store.** A module takes a key as a path string at run time, and it asserts that the path is outside the store. A certificate is public, so it can be a path in the store.

**The mTLS gate is `nixosModules.mtls-gate`.** nginx admits a request only with a client certificate that one CA signs. The firewall opens the port only to the listed source addresses.

| Option, under `tabnix.mtlsGate.<name>` | Sets |
| --- | --- |
| `address` and `port` | Where the gate listens |
| `upstream` | The URL of the service behind the gate, on the loopback address |
| `certificate` | The server certificate of the gate |
| `key` | The path of the server key at run time, outside the Nix store |
| `clientCa` | The CA that signs every client certificate that the gate admits |
| `allowFrom` | The source addresses or CIDR ranges that the firewall admits |

nginx runs as its own user. So the module passes each key to nginx as a systemd credential, and the key file can stay readable only by root.

A request without a valid client certificate gets status 400, and the upstream never sees it. A host that is not in `allowFrom` never reaches the port. The gate sets the headers `X-Client-Subject` and `X-Client-Verify` itself, so a client cannot send its own values. The module needs the nftables backend of the NixOS firewall.

## Consequences

A machine gets a fix of a module only when its repo moves to a new tag.

CI takes longer, because each test boots VMs. GitHub does not bill a public repo for these minutes.

On macOS, `just ci` skips the nixosTests, because they need Linux and KVM. CI on Linux still runs them.

## Rules

- A NixOS module is `nixosModules.<name>` from `nixos/<name>.nix`, and the flake-parts module never imports it.
- Every option of a NixOS module is under `tabnix.` and has a description.
- Every NixOS module has a nixosTest in `tests/<name>.nix` that asserts each refusal of the module. `(CI: ci)`
- The CI runner has KVM, so `just ci` runs every nixosTest on Linux. `(CI: ci)`
- A module takes a secret as a path at run time, outside the Nix store, and asserts it.
- An upstream behind `mtls-gate` listens only on the loopback address.
- A new NixOS module bumps the minor version.
