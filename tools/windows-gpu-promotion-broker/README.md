# Windows GPU promotion broker contract

This independently locked workspace defines the unprivileged request accepted
by a future separately privileged Windows service or remote HSM broker. The
normal `scribe-windows-gpu-promotion-client` binary validates that request in
memory and exits with code 78. It performs no filesystem or IPC operation and
has no signing key, ledger/state path, configurable broker endpoint, or fixture
mode.

The hostile-input copier, fixture Ed25519 authority, chained replay/epoch
ledger, signed receipt, recovery state machine, and atomic publisher are under
`cfg(test)` only. They prove the intended broker contract but are not deployable
production authority. In particular, the tests do not establish:

- a fixed authenticated service/HSM transport;
- service installation identity and ACLs;
- NT handle-relative traversal for every input component;
- non-resettable replay or security-epoch storage;
- a production key, trust root, CUDA inventory, or release catalog.

Production promotion must stay disabled until those controls are implemented
and independently reviewed. Run the current proof on Windows with:

```powershell
cargo test --locked --offline -- --test-threads=1
```
