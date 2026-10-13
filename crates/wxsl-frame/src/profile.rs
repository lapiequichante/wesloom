//! What a device can run, as data
//! ([ADR 0075](../../docs/adr/0075-webgl2-and-gl-are-a-fallback-profile.md)).
//!
//! Compute and storage are allowed: WebGPU and every native backend have
//! them, and the shipped pipelines use them. A [`Profile`] is how a backend
//! says it is *not* one of those — WebGL2, GLES 3.0, an old desktop GL, or
//! a Dawn compatibility device whose vertex stage cannot read storage — so
//! that the shared half, which both the wgpu and the Dawn renderer consume,
//! picks the fallbacks once:
//!
//! * instance rows move from storage buffers into data textures
//!   ([`Profile::macros`], `abi::FEATURE_INSTANCE_TEXTURE`);
//! * a compute effect runs its declared fragment arm
//!   ([`crate::effect::Downlevel::Fragment`]), lowered by the pipeline
//!   compiler into an ordinary screen pass;
//! * an effect that declared itself [`crate::effect::Downlevel::NativeOnly`]
//!   is refused by name when a pipeline is compiled, rather than failing on
//!   the device.
//!
//! The backend fills the profile from its own device; nothing here knows
//! which API that is.

use wxsl_core::abi;
use wxsl_core::macros::{MacroSet, MacroValue};

/// What the device a pipeline will run on can do, out of what the
/// shipped pipelines would use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Profile {
    /// Compute pipelines and storage-texture writes.
    pub compute: bool,
    /// Read-only storage buffers in the vertex *and* fragment stages — where
    /// the instance rows are read.
    pub storage_buffers: bool,
}

impl Default for Profile {
    fn default() -> Self {
        Profile::FULL
    }
}

impl Profile {
    /// WebGPU and every native backend: nothing falls back.
    pub const FULL: Profile = Profile {
        compute: true,
        storage_buffers: true,
    };

    /// WebGL2 and GLES 3.0, and so desktop GL 3.3: no compute, no storage.
    pub const WEBGL2: Profile = Profile {
        compute: false,
        storage_buffers: false,
    };

    /// Whether nothing falls back.
    pub fn is_full(&self) -> bool {
        *self == Profile::FULL
    }

    /// The ABI macros this profile pins over every material and every
    /// generated lighting pass — the renderer's to set, never a graph's.
    pub fn macros(&self) -> MacroSet {
        let mut macros = MacroSet::new();
        if !self.storage_buffers {
            macros.set(abi::FEATURE_INSTANCE_TEXTURE, MacroValue::Flag(true));
        }
        macros
    }

    /// A short name for messages: `full`, `webgl2`, or the missing parts.
    pub fn name(&self) -> String {
        match *self {
            Profile::FULL => "full".to_string(),
            Profile::WEBGL2 => "webgl2".to_string(),
            Profile { compute, .. } if !compute => "no compute".to_string(),
            _ => "no storage buffers".to_string(),
        }
    }
}

impl core::fmt::Display for Profile {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_profile_without_storage_reads_instances_from_textures() {
        assert!(Profile::FULL.macros().is_empty());
        let flag = Profile::WEBGL2.macros();
        assert_eq!(
            flag.get(abi::FEATURE_INSTANCE_TEXTURE),
            Some(MacroValue::Flag(true))
        );
        // A compatibility device: compute, but no vertex storage.
        let compat = Profile {
            compute: true,
            storage_buffers: false,
        };
        assert_eq!(compat.macros(), flag);
        assert_eq!(compat.name(), "no storage buffers");
    }
}
