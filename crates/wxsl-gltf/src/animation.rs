//! Skins and animations, sampled and applied on the host
//! ([ADR 0099](../../../docs/adr/0099-skins-and-animations-are-baked-into-a-bone-texture-and-skinned-in-the-vertex-stage.md)).
//!
//! The import keeps them as data: an [`Animation`] poses the document's
//! nodes at a time, and a [`Skin`] turns the posed joints into matrices.
//! [`BoneTexture::bake`] samples every clip once into one texture, which a
//! skinned material's vertex stage reads (`animation.skin`), so playing a
//! clip only moves the material's frame parameter. Nothing here needs a
//! device; the host uploads the texture.
//!
//! Joint and node indices are the *document's* nodes, not the file's.

use glam::{Mat4, Quat, Vec3, Vec4};
use wxsl_core::scene::{Scene, SceneNode};

use crate::ImportedScene;

/// A skeleton: the nodes a skinned mesh's joint indices name.
#[derive(Clone, Debug, PartialEq)]
pub struct Skin {
    /// The skin's name.
    pub name: String,
    /// Per joint, the document node that moves it.
    pub joints: Vec<usize>,
    /// Per joint, the inverse of its world matrix in the bind pose,
    /// column-major.
    pub inverse_bind: Vec<[f32; 16]>,
    /// The document node placing the meshes it skins. The joints' matrices
    /// are relative to it as imported, which keeps a clip's root motion.
    pub node: usize,
}

impl Skin {
    /// Each joint's matrix in `scene`'s current pose: from the skinned
    /// mesh's own space in the bind pose to its own space posed, relative
    /// to `mesh_world`, where the mesh's instance is drawn.
    pub fn joint_matrices(&self, scene: &Scene, mesh_world: &[f32; 16]) -> Vec<[f32; 16]> {
        let to_mesh = Mat4::from_cols_array(mesh_world).inverse();
        self.joints
            .iter()
            .zip(&self.inverse_bind)
            .map(|(&joint, inverse_bind)| {
                let world = Mat4::from_cols_array(&scene.node_world(joint));
                (to_mesh * world * Mat4::from_cols_array(inverse_bind)).to_cols_array()
            })
            .collect()
    }
}

/// One skin's joint matrices for every frame of every clip: what a skinned
/// material binds at [`crate::BONE_TEXTURE_SLOT`].
///
/// One row per frame, three texels per joint: the first three rows of its
/// matrix (an affine 3×4). Row 0 is the pose the file was imported in; each
/// clip follows, sampled at [`BoneTexture::fps`].
#[derive(Clone, Debug, PartialEq)]
pub struct BoneTexture {
    /// Texels per row: three per joint.
    pub width: u32,
    /// Rows: frames.
    pub height: u32,
    /// `width * height` RGBA texels, row by row.
    pub texels: Vec<[f32; 4]>,
    /// Frames per second the clips were sampled at.
    pub fps: f32,
    /// Per [`ImportedScene::animations`] clip, its first row and its frame
    /// count.
    pub clips: Vec<(u32, u32)>,
}

impl BoneTexture {
    /// Bake skin `skin` of `imported` through every clip at `fps`.
    pub fn bake(imported: &ImportedScene, skin: usize, fps: f32) -> Self {
        let skin = &imported.skins[skin];
        let rest = &imported.scene.nodes;
        let mesh_world = imported.scene.node_world(skin.node);
        let mut posed = imported.scene.clone();
        let mut texels = Vec::new();
        let row = |scene: &Scene, texels: &mut Vec<[f32; 4]>| {
            for matrix in skin.joint_matrices(scene, &mesh_world) {
                let matrix = Mat4::from_cols_array(&matrix);
                for index in 0..3 {
                    texels.push(matrix.row(index).to_array());
                }
            }
        };
        row(&posed, &mut texels);
        let mut height = 1;
        let mut clips = Vec::new();
        for animation in &imported.animations {
            let frames = (animation.duration * fps).ceil() as u32 + 1;
            clips.push((height, frames));
            for frame in 0..frames {
                animation.pose(frame as f32 / fps, rest, &mut posed.nodes);
                row(&posed, &mut texels);
            }
            posed.nodes.clone_from(rest);
            height += frames;
        }
        BoneTexture {
            width: skin.joints.len() as u32 * 3,
            height,
            texels,
            fps,
            clips,
        }
    }

    /// The row showing clip `clip` at `time` seconds, looping; row 0 for a
    /// clip the texture does not have.
    pub fn frame(&self, clip: usize, time: f32) -> f32 {
        let Some(&(first, frames)) = self.clips.get(clip) else {
            return 0.0;
        };
        let frame = (time * self.fps).rem_euclid(frames.max(1) as f32).floor();
        first as f32 + frame
    }
}

/// A clip: keyframed node properties.
#[derive(Clone, Debug, PartialEq)]
pub struct Animation {
    /// The clip's name.
    pub name: String,
    /// The tracks.
    pub channels: Vec<Channel>,
    /// The last keyframe's time, in seconds.
    pub duration: f32,
}

/// One node property's keyframes.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    /// The document node it moves.
    pub node: usize,
    /// What it sets.
    pub property: Property,
    /// How it reads between keyframes.
    pub interpolation: Interpolation,
    /// Keyframe times, in seconds, increasing.
    pub times: Vec<f32>,
    /// Per keyframe, `[x, y, z, 0]` or a quaternion `[x, y, z, w]`; for
    /// [`Interpolation::CubicSpline`], three per keyframe: in-tangent,
    /// value, out-tangent.
    pub values: Vec<[f32; 4]>,
}

/// The property a [`Channel`] animates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Property {
    /// The node's translation.
    Translation,
    /// The node's rotation, a unit quaternion.
    Rotation,
    /// The node's scale.
    Scale,
}

/// How a [`Channel`] reads between keyframes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interpolation {
    /// Linear, and spherical for rotations.
    Linear,
    /// The previous keyframe's value.
    Step,
    /// A cubic Hermite spline through the keyframes.
    CubicSpline,
}

impl Animation {
    /// Pose `nodes` at `time` (seconds, clamped to the clip): each animated
    /// node takes its `rest` transform with the animated properties
    /// replaced. `rest` is the document's nodes as imported.
    pub fn pose(&self, time: f32, rest: &[SceneNode], nodes: &mut [SceneNode]) {
        let mut posed: Vec<(usize, Vec3, Quat, Vec3)> = Vec::new();
        for channel in &self.channels {
            let Some(rest) = rest.get(channel.node) else {
                continue;
            };
            let at = match posed.iter().position(|(node, ..)| *node == channel.node) {
                Some(at) => at,
                None => {
                    let (scale, rotation, translation) =
                        Mat4::from_cols_array(&rest.transform).to_scale_rotation_translation();
                    posed.push((channel.node, translation, rotation, scale));
                    posed.len() - 1
                }
            };
            let value = channel.sample(time);
            let (_, translation, rotation, scale) = &mut posed[at];
            match channel.property {
                Property::Translation => *translation = value.truncate(),
                Property::Rotation => *rotation = Quat::from_vec4(value).normalize(),
                Property::Scale => *scale = value.truncate(),
            }
        }
        for (node, translation, rotation, scale) in posed {
            if let Some(node) = nodes.get_mut(node) {
                node.transform =
                    Mat4::from_scale_rotation_translation(scale, rotation, translation)
                        .to_cols_array();
            }
        }
    }
}

impl Channel {
    /// The value at `time`, clamped to the first and last keyframes.
    pub fn sample(&self, time: f32) -> Vec4 {
        let cubic = self.interpolation == Interpolation::CubicSpline;
        let value = |key: usize| {
            let index = if cubic { key * 3 + 1 } else { key };
            Vec4::from_array(self.values.get(index).copied().unwrap_or_default())
        };
        let last = self.times.len().saturating_sub(1);
        // The first keyframe after `time`.
        let next = self.times.partition_point(|&key| key <= time);
        if next == 0 || self.times.len() < 2 {
            return value(0);
        }
        if next > last {
            return value(last);
        }
        let previous = next - 1;
        let span = self.times[next] - self.times[previous];
        let t = if span > 0.0 {
            (time - self.times[previous]) / span
        } else {
            0.0
        };
        match self.interpolation {
            Interpolation::Step => value(previous),
            Interpolation::Linear if self.property == Property::Rotation => {
                let (a, b) = (
                    Quat::from_vec4(value(previous)),
                    Quat::from_vec4(value(next)),
                );
                Vec4::from(a.slerp(b, t))
            }
            Interpolation::Linear => value(previous).lerp(value(next), t),
            Interpolation::CubicSpline => {
                let tangent = |index: usize| {
                    Vec4::from_array(self.values.get(index).copied().unwrap_or_default()) * span
                };
                let (p0, m0) = (value(previous), tangent(previous * 3 + 2));
                let (p1, m1) = (value(next), tangent(next * 3));
                let (t2, t3) = (t * t, t * t * t);
                p0 * (2.0 * t3 - 3.0 * t2 + 1.0)
                    + m0 * (t3 - 2.0 * t2 + t)
                    + p1 * (-2.0 * t3 + 3.0 * t2)
                    + m1 * (t3 - t2)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(property: Property, interpolation: Interpolation, values: Vec<[f32; 4]>) -> Channel {
        Channel {
            node: 0,
            property,
            interpolation,
            times: vec![0.0, 1.0],
            values,
        }
    }

    #[test]
    fn channels_interpolate_and_clamp() {
        let linear = channel(
            Property::Translation,
            Interpolation::Linear,
            vec![[0.0; 4], [2.0, 4.0, 6.0, 0.0]],
        );
        assert_eq!(linear.sample(0.5), Vec4::new(1.0, 2.0, 3.0, 0.0));
        assert_eq!(linear.sample(-1.0), Vec4::ZERO);
        assert_eq!(linear.sample(9.0), Vec4::new(2.0, 4.0, 6.0, 0.0));
        let step = Channel {
            interpolation: Interpolation::Step,
            ..linear
        };
        assert_eq!(step.sample(0.99), Vec4::ZERO);
    }

    #[test]
    fn rotations_slerp() {
        let quarter = Quat::from_rotation_y(core::f32::consts::FRAC_PI_2);
        let rotation = channel(
            Property::Rotation,
            Interpolation::Linear,
            vec![Quat::IDENTITY.to_array(), quarter.to_array()],
        );
        let half = Quat::from_vec4(rotation.sample(0.5));
        let expected = Quat::from_rotation_y(core::f32::consts::FRAC_PI_4);
        assert!(half.angle_between(expected) < 1e-4, "{half:?}");
    }

    #[test]
    fn a_joint_matrix_is_relative_to_the_mesh_and_its_bind_pose() {
        // The joint was bound at the mesh's origin, the mesh is placed at
        // x = 1, and the joint is now at x = 3: in the mesh's space it
        // moved by 2.
        let mut scene = Scene::new("skin");
        let mesh = scene.add_node(
            SceneNode::new("mesh").with_transform(Mat4::from_translation(Vec3::X).to_cols_array()),
        );
        let joint = scene.add_node(
            SceneNode::new("joint")
                .with_transform(Mat4::from_translation(Vec3::X * 3.0).to_cols_array()),
        );
        let skin = Skin {
            name: "skin".to_string(),
            joints: vec![joint],
            inverse_bind: vec![Mat4::IDENTITY.to_cols_array()],
            node: mesh,
        };
        let [matrix] = skin.joint_matrices(&scene, &scene.node_world(mesh))[..] else {
            panic!("one joint");
        };
        let moved = Mat4::from_cols_array(&matrix).transform_point3(Vec3::ZERO);
        assert!((moved - Vec3::X * 2.0).length() < 1e-5, "{moved:?}");
    }

    #[test]
    fn posing_keeps_the_properties_a_clip_leaves_alone() {
        let rest = vec![SceneNode::new("joint").with_transform(
            Mat4::from_scale_rotation_translation(Vec3::splat(2.0), Quat::IDENTITY, Vec3::X)
                .to_cols_array(),
        )];
        let animation = Animation {
            name: "lift".to_string(),
            channels: vec![channel(
                Property::Translation,
                Interpolation::Linear,
                vec![[0.0; 4], [0.0, 2.0, 0.0, 0.0]],
            )],
            duration: 1.0,
        };
        let mut nodes = rest.clone();
        animation.pose(0.5, &rest, &mut nodes);
        let (scale, _, translation) =
            Mat4::from_cols_array(&nodes[0].transform).to_scale_rotation_translation();
        assert!((scale - Vec3::splat(2.0)).length() < 1e-5);
        assert!((translation - Vec3::Y).length() < 1e-5);
    }
}
