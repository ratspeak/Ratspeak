# btleplug Android bridge

The Java sources under `com/nonpolynomial/btleplug` and
`io/github/gedgygedgy/rust` in the Android app are copied without modification
from [btleplug 0.13.4](https://github.com/deviceplug/btleplug/tree/0.13.4/src/droidplug/java).
`source.json` records the exact revision and SHA-256 inventory. The upstream
BSD 3-Clause license is included in `LICENSE.md`; source headers retain their
copyright notices.

Update the Rust dependency, JNI initializer, complete Java inventory and this
manifest together. Run `python3 scripts/ci/check-btleplug-integration.py` and the
minified Android package checks after changing the integration. Existing native
Android BLE owners continue to use JNI 0.19; only btleplug uses the isolated
`jni-btleplug` 0.22 bridge.

`jni-boundaries.json` records method descriptors from these exact Java sources,
compiled with JDK 17 against Android API 36 and inspected with `javap -p -s`.
The CI and release archive gates verify those definitions in the final minified
DEX, including the native registration signatures and exception constructors.
The upstream license is bundled in the application resources.
