# Python bindings

The Python package lives in [`src-python/`](../src-python/README.md). Its
documentation is maintained alongside the bindings:

- [Getting started](../src-python/docs/README.md)
- [Installation and building](../src-python/docs/installation.md)
- [Media reading and export guide](../src-python/docs/guide.md)
- [API reference](../src-python/docs/reference.md)
- [Build, FFI, and API tests](../src-python/docs/testing.md)

X5 exports accept
`StitchConfig(stabilization=Stabilization.DIRECTION_LOCK, rolling_shutter=RollingShutterCorrection.AUTO)`.
`FLOW_STATE` preserves camera heading while leveling; `OFF` bypasses motion.
Required readout correction is selected with
`RollingShutterCorrection.REQUIRED`. For asynchronous exports,
`job.stabilization()` returns the prepared motion description and
`job.take_warnings()` reports omissions. See
[motion conventions](stabilization.md).
