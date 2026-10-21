//! Literal provenance bytes for non-executing macOS package inspection.

macro_rules! retain_macos_marker {
    ($name:ident, $value:expr) => {
        #[used]
        // SAFETY: this is inert byte data in an ordinary Mach-O data section,
        // not instructions, pointers, an initializer or a runtime authority.
        // `used` preserves compiler emission; `no_dead_strip` preserves linking.
        #[unsafe(link_section = "__DATA,__scribe_id,regular,no_dead_strip")]
        static $name: [u8; ($value).len()] = {
            let source = ($value).as_bytes();
            let mut bytes = [0; ($value).len()];
            let mut index = 0;
            while index < source.len() {
                bytes[index] = source[index];
                index += 1;
            }
            bytes
        };
    };
}

pub(super) use retain_macos_marker;
