# seekdeep-process-exit-hook

Internal native support for synchronous managed-process cleanup during normal
host exit. Rust's standard library does not expose `atexit`, so this crate owns
the workspace's single narrowly documented unsafe platform call.

Registrations hold targets weakly and can be removed repeatedly without side
effects. One process-global C callback upgrades the registrations still alive,
invokes each target synchronously, and contains every target panic so a failure
cannot prevent later targets from receiving finalization. The local subprocess
provider registers before publication and unregisters only after its awaited
normal disposal reaches quiescence.

This callback is best effort, performs no asynchronous work, and does not claim
that the terminated processes have been reaped. Exit paths that cannot execute
`atexit` callbacks still require an external supervisor.
