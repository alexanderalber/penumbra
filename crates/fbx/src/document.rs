//! Objects, connections and properties: the layer above the node tree.
//!
//! An FBX scene is a flat list of objects under the `Objects` record, each with a 64-bit id, a
//! class (the record's name: `Model`, `Geometry`, `Material`, ...), a sub-class (`Mesh`,
//! `LimbNode`, ...) and a name. The `Connections` record links them: `OO` connects an object to
//! an object (a geometry to its model, a model to its parent), `OP` an object to a property of
//! another (a texture to a material's `DiffuseColor`). Id 0 is the scene root.
//!
//! Properties live in `Properties70` records; a property the object leaves out takes its value
//! from the property template of its class in `Definitions`, not from a constant.

use std::collections::HashMap;
use std::sync::Arc;

use crate::tree::{self, Node, Tree, Value};
use crate::{Error, Limits};

/// One property: `P: name, type, label, flags, values...`.
#[derive(Clone, Debug, PartialEq)]
pub struct Property {
    pub name: String,
    /// E.g. `Lcl Translation`, `Color`, `double`, `enum`.
    pub type_name: String,
    pub label: String,
    /// `A` animatable, `U` user-defined, `+` animated, ...
    pub flags: String,
    pub values: Vec<Value>,
}

impl Property {
    pub fn number(&self) -> Option<f64> {
        self.values.first()?.as_f64().filter(|x| x.is_finite())
    }

    pub fn int(&self) -> Option<i64> {
        self.values.first()?.as_i64()
    }

    pub fn string(&self) -> Option<&str> {
        self.values.first()?.as_str()
    }

    /// Three numbers, e.g. a translation or a colour.
    pub fn vec3(&self) -> Option<[f64; 3]> {
        let mut out = [0.0; 3];
        for (o, v) in out.iter_mut().zip(self.values.iter()) {
            *o = v.as_f64().filter(|x| x.is_finite())?;
        }
        (self.values.len() >= 3).then_some(out)
    }
}

/// An object's own properties, with its class's template behind them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Properties {
    pub own: Vec<Property>,
    pub defaults: Option<Arc<Vec<Property>>>,
}

impl Properties {
    /// The property called `name`: the object's own, else the template's. A name listed twice
    /// takes its last entry (as ufbx does).
    pub fn get(&self, name: &str) -> Option<&Property> {
        self.own
            .iter()
            .rfind(|p| p.name == name)
            .or_else(|| self.defaults.as_ref().and_then(|d| d.iter().rfind(|p| p.name == name)))
    }

    pub fn number(&self, name: &str) -> Option<f64> {
        self.get(name)?.number()
    }

    pub fn int(&self, name: &str) -> Option<i64> {
        self.get(name)?.int()
    }

    pub fn string(&self, name: &str) -> Option<&str> {
        self.get(name)?.string()
    }

    pub fn vec3(&self, name: &str) -> Option<[f64; 3]> {
        self.get(name)?.vec3()
    }

    /// Reads a `Properties70` record (older `Properties60` records hold `Property` entries in
    /// the same layout).
    pub fn read(node: &Node) -> Properties {
        let mut own = Vec::new();
        for p in node.children.iter().filter(|c| c.name == "P" || c.name == "Property") {
            let text = |i: usize| p.value(i).and_then(Value::as_str).unwrap_or("").to_string();
            let Some(name) = p.value(0).and_then(Value::as_str) else { continue };
            own.push(Property {
                name: name.to_string(),
                type_name: text(1),
                label: text(2),
                flags: text(3),
                values: p.values.get(4..).map_or_else(Vec::new, <[Value]>::to_vec),
            });
        }
        Properties { own, defaults: None }
    }
}

/// One object of the `Objects` record.
#[derive(Clone, Debug, PartialEq)]
pub struct Object {
    pub id: i64,
    /// The record's name: `Model`, `Geometry`, `Material`, `Texture`, `Video`, `Deformer`,
    /// `AnimationCurve`, ...
    pub class: String,
    /// `Mesh`, `LimbNode`, `Null`, `Skin`, `Cluster`, ... (an `Fbx` prefix removed).
    pub sub_class: String,
    pub name: String,
    pub props: Properties,
    /// Index of the object's record among the children of `Objects`.
    pub node: usize,
}

/// How a connection links its two ends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConnectionKind {
    /// `OO`: object to object.
    ObjectObject,
    /// `OP`: object to a property of the parent.
    ObjectProperty,
    /// `PO`: a property of the child to the parent object.
    PropertyObject,
    /// `PP`: property to property.
    PropertyProperty,
}

/// One `C` record: `child` is connected into `parent` (a geometry into its model, a model into
/// its parent model).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Connection {
    pub kind: ConnectionKind,
    pub child: i64,
    pub parent: i64,
    /// The parent's property for `OP` and `PP`, the child's for `PO`.
    pub property: Option<String>,
    /// The parent's property for `PP`.
    pub parent_property: Option<String>,
}

/// A parsed file: the node tree, the objects by id, the connections both ways, and the
/// `GlobalSettings` properties.
#[derive(Clone, Debug, Default)]
pub struct Document {
    pub tree: Tree,
    pub objects: Vec<Object>,
    pub connections: Vec<Connection>,
    pub settings: Properties,
    by_id: HashMap<i64, usize>,
    /// Parent id to the indices of connections into it, in file order.
    into: HashMap<i64, Vec<usize>>,
    /// Child id to the indices of connections from it, in file order.
    from: HashMap<i64, Vec<usize>>,
}

/// An object's name and class from `Name\0\x01Class` (binary) or `Class::Name` (ASCII).
pub fn split_name(raw: &str) -> (&str, &str) {
    if let Some((name, class)) = raw.split_once("\u{0}\u{1}") {
        (name, class)
    } else if let Some((class, name)) = raw.split_once("::") {
        (name, class)
    } else {
        (raw, "")
    }
}

fn strip_fbx(s: &str) -> &str {
    s.strip_prefix("Fbx").filter(|r| !r.is_empty()).unwrap_or(s)
}

impl Document {
    /// Reads an FBX file, binary or ASCII, into a document.
    pub fn parse(bytes: &[u8], limits: &Limits) -> Result<Document, Error> {
        Self::from_tree(tree::parse(bytes, limits)?, limits)
    }

    pub fn from_tree(tree: Tree, limits: &Limits) -> Result<Document, Error> {
        // Templates: (object class, template sub-class without `Fbx`) to properties.
        let mut templates: Vec<(String, String, Arc<Vec<Property>>)> = Vec::new();
        if let Some(defs) = tree.top("Definitions") {
            for t in defs.children_named("ObjectType") {
                let Some(class) = t.value(0).and_then(Value::as_str) else { continue };
                for pt in t.children_named("PropertyTemplate") {
                    let sub = strip_fbx(pt.value(0).and_then(Value::as_str).unwrap_or("")).to_string();
                    if let Some(p70) = pt.child("Properties70") {
                        templates.push((class.to_string(), sub, Arc::new(Properties::read(p70).own)));
                    }
                }
            }
        }
        let template = |class: &str, sub: &str| -> Option<Arc<Vec<Property>>> {
            // Materials, models, stacks and layers take their class's template whatever their
            // sub-class; the rest need both to match.
            let any = matches!(class, "Material" | "Model" | "AnimationStack" | "AnimationLayer");
            templates.iter().find(|(c, s, _)| c == class && (any || s == sub)).map(|(_, _, p)| p.clone())
        };
        let mut objects = Vec::new();
        let mut by_id = HashMap::new();
        if let Some(objs) = tree.top("Objects") {
            if objs.children.len() > limits.max_objects {
                return Err(Error::new(format!("more than {} objects", limits.max_objects)));
            }
            for (k, o) in objs.children.iter().enumerate() {
                // Objects of other shapes (a few writers mix some in) are skipped.
                let (Some(id), Some(raw)) =
                    (o.value(0).and_then(Value::as_i64), o.value(1).and_then(Value::as_str))
                else {
                    continue;
                };
                let (name, _) = split_name(raw);
                let sub_class = strip_fbx(o.value(2).and_then(Value::as_str).unwrap_or("")).to_string();
                let mut props = o.child("Properties70").map(Properties::read).unwrap_or_default();
                props.defaults = template(&o.name, &sub_class);
                if by_id.contains_key(&id) {
                    continue;
                }
                by_id.insert(id, objects.len());
                objects.push(Object {
                    id,
                    class: o.name.clone(),
                    sub_class,
                    name: name.to_string(),
                    props,
                    node: k,
                });
            }
        }
        let mut connections = Vec::new();
        let mut seen = std::collections::HashSet::new();
        if let Some(conns) = tree.top("Connections") {
            if conns.children.len() > limits.max_connections {
                return Err(Error::new(format!("more than {} connections", limits.max_connections)));
            }
            for c in conns.children_named("C") {
                let (Some(kind), Some(child), Some(parent)) = (
                    c.value(0).and_then(Value::as_str),
                    c.value(1).and_then(Value::as_i64),
                    c.value(2).and_then(Value::as_i64),
                ) else {
                    continue;
                };
                let kind = match kind {
                    "OO" => ConnectionKind::ObjectObject,
                    "OP" => ConnectionKind::ObjectProperty,
                    "PO" => ConnectionKind::PropertyObject,
                    "PP" => ConnectionKind::PropertyProperty,
                    _ => continue,
                };
                let text = |i: usize| c.value(i).and_then(Value::as_str).map(str::to_string);
                let conn = Connection { kind, child, parent, property: text(3), parent_property: text(4) };
                // A connection listed twice counts once.
                if seen.insert(conn.clone()) {
                    connections.push(conn);
                }
            }
        }
        let mut into: HashMap<i64, Vec<usize>> = HashMap::new();
        let mut from: HashMap<i64, Vec<usize>> = HashMap::new();
        for (i, c) in connections.iter().enumerate() {
            into.entry(c.parent).or_default().push(i);
            from.entry(c.child).or_default().push(i);
        }
        let settings = tree
            .top("GlobalSettings")
            .and_then(|g| g.child("Properties70"))
            .map(Properties::read)
            .unwrap_or_default();
        Ok(Document { tree, objects, connections, settings, by_id, into, from })
    }

    pub fn version(&self) -> u32 {
        self.tree.version
    }

    pub fn object(&self, id: i64) -> Option<&Object> {
        self.objects.get(*self.by_id.get(&id)?)
    }

    /// The record of `object`, for what is not a property (a geometry's arrays).
    pub fn node(&self, object: &Object) -> Option<&Node> {
        self.tree.top("Objects")?.children.get(object.node)
    }

    /// Connections into `parent`, in file order.
    pub fn connections_into(&self, parent: i64) -> impl Iterator<Item = &Connection> + '_ {
        self.into.get(&parent).into_iter().flatten().filter_map(|i| self.connections.get(*i))
    }

    /// Connections from `child`, in file order.
    pub fn connections_from(&self, child: i64) -> impl Iterator<Item = &Connection> + '_ {
        self.from.get(&child).into_iter().flatten().filter_map(|i| self.connections.get(*i))
    }

    /// Objects of `class` connected object-to-object into `parent`, in file order.
    pub fn sources<'a>(&'a self, parent: i64, class: &'a str) -> impl Iterator<Item = &'a Object> + 'a {
        self.connections_into(parent)
            .filter(|c| c.kind == ConnectionKind::ObjectObject)
            .filter_map(|c| self.object(c.child))
            .filter(move |o| o.class == class)
    }

    /// Objects of `class` that `child` is connected into, object to object, in file order.
    pub fn destinations<'a>(&'a self, child: i64, class: &'a str) -> impl Iterator<Item = &'a Object> + 'a {
        self.connections_from(child)
            .filter(|c| c.kind == ConnectionKind::ObjectObject)
            .filter_map(|c| self.object(c.parent))
            .filter(move |o| o.class == class)
    }

    /// Objects connected into property `property` of `parent` (a material's `DiffuseColor`
    /// texture).
    pub fn property_sources<'a>(
        &'a self,
        parent: i64,
        property: &'a str,
    ) -> impl Iterator<Item = &'a Object> + 'a {
        self.connections_into(parent)
            .filter(move |c| {
                c.kind == ConnectionKind::ObjectProperty && c.property.as_deref() == Some(property)
            })
            .filter_map(|c| self.object(c.child))
    }
}
