//! The scene as a user sees it: meshes placed in the world, their materials and textures, and
//! the file's units and axes.

use crate::anim::Animation;
use crate::document::{Document, Object, Properties};
use crate::geometry::Geometry;
use crate::skin::{Skin, skins_of};
use crate::transform::{Hierarchy, Mat4, NodeWorld};
use crate::tree::Value;
use crate::{Error, Limits};

/// A signed coordinate axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    /// +x.
    PosX,
    /// -x.
    NegX,
    /// +y.
    PosY,
    /// -y.
    NegY,
    /// +z.
    PosZ,
    /// -z.
    NegZ,
}

impl Axis {
    /// From `GlobalSettings`' axis number (0 x, 1 y, 2 z) and sign.
    pub fn from_index(axis: i64, sign: i64) -> Option<Axis> {
        Some(match (axis, sign >= 0) {
            (0, true) => Axis::PosX,
            (0, false) => Axis::NegX,
            (1, true) => Axis::PosY,
            (1, false) => Axis::NegY,
            (2, true) => Axis::PosZ,
            (2, false) => Axis::NegZ,
            _ => return None,
        })
    }

    /// The unit vector along the axis.
    pub fn vector(self) -> [f64; 3] {
        match self {
            Axis::PosX => [1.0, 0.0, 0.0],
            Axis::NegX => [-1.0, 0.0, 0.0],
            Axis::PosY => [0.0, 1.0, 0.0],
            Axis::NegY => [0.0, -1.0, 0.0],
            Axis::PosZ => [0.0, 0.0, 1.0],
            Axis::NegZ => [0.0, 0.0, -1.0],
        }
    }

    fn component(self) -> usize {
        match self {
            Axis::PosX | Axis::NegX => 0,
            Axis::PosY | Axis::NegY => 1,
            Axis::PosZ | Axis::NegZ => 2,
        }
    }
}

/// Which way the file's coordinates point: `right`, `up` and `front` (the direction a model's
/// front faces). The FBX SDK's default is x right, y up, z front.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Axes {
    /// The axis pointing right.
    pub right: Axis,
    /// The axis pointing up.
    pub up: Axis,
    /// The axis a model's front faces along.
    pub front: Axis,
}

impl Axes {
    /// The FBX SDK's default: x right, y up, z front.
    pub const Y_UP: Axes = Axes { right: Axis::PosX, up: Axis::PosY, front: Axis::PosZ };

    /// From `CoordAxis`, `UpAxis` and `FrontAxis` with their signs; `None` when one is missing
    /// or two share an axis.
    pub fn from_settings(settings: &Properties) -> Option<Axes> {
        let axis = |a: &str, s: &str| Axis::from_index(settings.int(a)?, settings.int(s).unwrap_or(1));
        let axes = Axes {
            right: axis("CoordAxis", "CoordAxisSign")?,
            up: axis("UpAxis", "UpAxisSign")?,
            front: axis("FrontAxis", "FrontAxisSign")?,
        };
        let (r, u, f) = (axes.right.component(), axes.up.component(), axes.front.component());
        (r != u && u != f && r != f).then_some(axes)
    }

    /// The rotation (or mirror, when one basis is left-handed and the other not) that takes
    /// these axes onto `target`'s: the file's right becomes the target's right, and so on.
    pub fn conversion_to(&self, target: &Axes) -> Mat4 {
        let from = [self.right, self.up, self.front].map(Axis::vector);
        let to = [target.right, target.up, target.front].map(Axis::vector);
        // M = sum over k of to_k * from_k^T (both bases orthonormal).
        let mut m = crate::transform::IDENTITY;
        for (i, row) in m.iter_mut().take(3).enumerate() {
            for (j, x) in row.iter_mut().take(3).enumerate() {
                *x = from
                    .iter()
                    .zip(&to)
                    .map(|(f, t)| t.get(i).copied().unwrap_or(0.0) * f.get(j).copied().unwrap_or(0.0))
                    .sum();
            }
        }
        m
    }
}

/// A geometry placed by a model.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct MeshInstance {
    /// Index into [`Hierarchy::nodes`].
    pub node: usize,
    /// The model's object id.
    pub model_id: i64,
    /// The model's name.
    pub name: String,
    /// The geometry's object id: pass it to [`Scene::geometry`] and [`Scene::skin`].
    pub geometry_id: i64,
    /// Geometry to world, in the file's units and axes.
    pub geometry_to_world: Mat4,
    /// The model's materials in connection order: what the geometry's material indices refer
    /// to.
    pub materials: Vec<i64>,
}

/// A material's colour and diffuse texture.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct MaterialInfo {
    /// The material's object id.
    pub id: i64,
    /// The material's name.
    pub name: String,
    /// `DiffuseColor` (or the older `Diffuse`) times `DiffuseFactor`, as stored (linear).
    pub diffuse: [f64; 3],
    /// `Opacity` where the material has one, else 1.
    pub opacity: f64,
    /// The texture on `DiffuseColor` (or `Diffuse`), if any.
    pub diffuse_texture: Option<TextureInfo>,
}

/// A texture and where its image is.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct TextureInfo {
    /// The texture's object id.
    pub id: i64,
    /// The texture's name.
    pub name: String,
    /// The path as the writer saw it, usually absolute on the writer's machine.
    pub file_name: String,
    /// The path relative to the FBX file.
    pub relative_file_name: String,
    /// The `Video` object whose `Content` holds the embedded image, if any.
    pub video_id: Option<i64>,
}

/// A parsed file with its hierarchy evaluated.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Scene {
    /// The objects, connections and settings below the scene.
    pub document: Document,
    /// The models and their parents.
    pub hierarchy: Hierarchy,
    /// Per entry of [`Hierarchy::nodes`], from the file's values.
    pub worlds: Vec<NodeWorld>,
    /// Metres per file unit: `UnitScaleFactor` (centimetres per unit) / 100.
    pub unit_meters: f64,
    /// The file's axes; `None` when `GlobalSettings` does not say (most such files are y up).
    pub axes: Option<Axes>,
    /// The takes and their curves; [`Animation::worlds`] places the models at a time.
    pub animation: Animation,
    /// What was read in a doubtful way or left out, in words. A file with warnings still loads.
    pub warnings: Vec<String>,
}

impl Scene {
    /// Reads a binary or ASCII FBX 7.x file from its bytes.
    pub fn parse(bytes: &[u8], limits: &Limits) -> Result<Scene, Error> {
        Self::from_document(Document::parse(bytes, limits)?, limits)
    }

    /// The scene of a document read earlier.
    pub fn from_document(document: Document, limits: &Limits) -> Result<Scene, Error> {
        let mut warnings = Vec::new();
        let hierarchy = Hierarchy::from_document(&document, limits, &mut warnings)?;
        let worlds = hierarchy.evaluate_static();
        let unit = document.settings.number("UnitScaleFactor").filter(|u| *u > 0.0).unwrap_or(1.0);
        let axes = Axes::from_settings(&document.settings);
        let animation = Animation::read(&document, limits, &mut warnings)?;
        Ok(Scene { unit_meters: unit / 100.0, axes, document, hierarchy, worlds, animation, warnings })
    }

    /// Every model with a mesh geometry, in hierarchy order. A model with several geometries
    /// gives one instance each.
    pub fn mesh_instances(&self) -> Vec<MeshInstance> {
        let doc = &self.document;
        let mut out = Vec::new();
        for (i, node) in self.hierarchy.nodes.iter().enumerate() {
            let Some(world) = self.worlds.get(i) else { continue };
            let materials: Vec<i64> = doc.sources(node.id, "Material").map(|m| m.id).collect();
            for g in
                doc.sources(node.id, "Geometry").filter(|g| g.sub_class == "Mesh" || g.sub_class.is_empty())
            {
                out.push(MeshInstance {
                    node: i,
                    model_id: node.id,
                    name: node.name.clone(),
                    geometry_id: g.id,
                    geometry_to_world: world.geometry,
                    materials: materials.clone(),
                });
            }
        }
        out
    }

    /// Reads geometry `id`.
    pub fn geometry(&self, id: i64, limits: &Limits) -> Result<Geometry, Error> {
        let obj = self.document.object(id).ok_or_else(|| Error::new(format!("no object {id}")))?;
        let node = self.document.node(obj).ok_or_else(|| Error::new(format!("no record for object {id}")))?;
        Geometry::read(node, limits).map_err(|e| e.context(format!("geometry `{}`", obj.name)))
    }

    /// The skin of geometry `id`, if it has one, read against the first model that places the
    /// geometry. A geometry with several skins gets the first, with a warning.
    pub fn skin(&self, id: i64, limits: &Limits) -> Result<Option<Skin>, Error> {
        let doc = &self.document;
        let mut skins = skins_of(doc, id);
        let Some(first) = skins.next() else { return Ok(None) };
        let more = skins.next().is_some();
        let geometry = doc.object(id).and_then(|g| doc.node(g));
        let points = geometry.and_then(|g| g.child_array("Vertices")).map_or(0, |a| a.len() / 3);
        let node =
            doc.destinations(id, "Model").find_map(|m| self.hierarchy.index_of(m.id)).unwrap_or(usize::MAX);
        let mut skin = Skin::read(doc, &self.hierarchy, &self.worlds, first, node, points, limits)?;
        if more {
            skin.warnings.push(format!(
                "geometry `{}` has several skins; only `{}` is used",
                doc.object(id).map_or("", |g| g.name.as_str()),
                skin.name
            ));
        }
        Ok(Some(skin))
    }

    /// Material `id`'s colour, opacity and diffuse texture; `None` when no material has the id.
    pub fn material(&self, id: i64) -> Option<MaterialInfo> {
        let doc = &self.document;
        let m = doc.object(id)?;
        let p = &m.props;
        let color = p.vec3("DiffuseColor").or_else(|| p.vec3("Diffuse")).unwrap_or([0.8; 3]);
        let factor = p.number("DiffuseFactor").unwrap_or(1.0);
        let opacity = p.own.iter().find(|q| q.name == "Opacity").and_then(|q| q.number()).unwrap_or(1.0);
        let texture = doc
            .property_sources(id, "DiffuseColor")
            .chain(doc.property_sources(id, "Diffuse"))
            .find(|t| t.class == "Texture")
            .map(|t| self.texture(t));
        Some(MaterialInfo {
            id,
            name: m.name.clone(),
            diffuse: color.map(|c| c * factor),
            opacity: opacity.clamp(0.0, 1.0),
            diffuse_texture: texture,
        })
    }

    fn texture(&self, t: &Object) -> TextureInfo {
        let doc = &self.document;
        let rec = doc.node(t);
        let text = |name: &str| rec.and_then(|r| r.child_str(name)).unwrap_or("").to_string();
        let video = doc.sources(t.id, "Video").next();
        let mut relative = text("RelativeFilename");
        if relative.is_empty() {
            relative = video
                .and_then(|v| doc.node(v))
                .and_then(|r| r.child_str("RelativeFilename"))
                .unwrap_or("")
                .to_string();
        }
        let mut file_name = text("FileName");
        if file_name.is_empty() {
            file_name = t.props.string("FileName").unwrap_or("").to_string();
        }
        TextureInfo {
            id: t.id,
            name: t.name.clone(),
            file_name,
            relative_file_name: relative,
            video_id: video.map(|v| v.id),
        }
    }

    /// The embedded bytes of `Video` object `id`; `None` when it has none.
    pub fn video_content(&self, id: i64) -> Option<&[u8]> {
        let doc = &self.document;
        let rec = doc.node(doc.object(id)?)?;
        let bytes = rec.child("Content")?.values.iter().find_map(|v| match v {
            Value::Raw(b) => Some(b.as_slice()),
            _ => None,
        })?;
        (!bytes.is_empty()).then_some(bytes)
    }
}
