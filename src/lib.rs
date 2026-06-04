use bevy::{
    ecs::component::{ComponentCloneBehavior, ComponentDescriptor, ComponentId, StorageType},
    prelude::*,
    ptr::OwningPtr,
};
use lasso::{Rodeo, Spur};
use mluau::prelude::*;
use std::{
    alloc::{Layout, alloc_zeroed, dealloc},
    collections::HashMap,
    ptr::NonNull,
};

pub struct ScriptingPlugin;

impl Plugin for ScriptingPlugin {
    fn build(&self, app: &mut App) {
        app.init_non_send::<ScriptingRuntime>()
            .init_non_send::<EngineStringPool>()
            .init_resource::<SchemaRegistry>();
    }
}

pub enum LuaSchedule {
    Startup,
    Update,
}

pub enum Access {
    With(ComponentId),
    Read(ComponentId),
    Write(ComponentId),
    Optional(Box<Access>),
}

pub struct DynamicQuery {
    terms: Vec<Access>,
}

pub enum LuaParamKind {
    Query(DynamicQuery),
    Resource,
}

#[derive(Default)]
pub struct ScriptingRuntime {
    pub lua: Lua,
    pub systems: Vec<LuaSystemDescriptor>,
    pub schemas: HashMap<ComponentId, DynamicComponentSchema>,
}

pub struct LuaSystemDescriptor {
    func: LuaFunction,
    schedule: LuaSchedule,
    params: Vec<LuaParamKind>,
}

#[derive(Default)]
pub struct EngineStringPool {
    pub rodeo: Rodeo,
    pub bridge: HashMap<Spur, LuaString>,
}

impl EngineStringPool {
    pub fn get_lua_str(&self, spur: Spur) -> &LuaString {
        self.bridge.get(&spur).expect("unregistered spur")
    }

    pub fn register_lua_string(&mut self, lua: &Lua, s: &LuaString) -> Option<Spur> {
        let borrowed = s.to_str().ok()?;
        let spur = self.rodeo.get_or_intern(&*borrowed);
        self.bridge
            .entry(spur)
            .or_insert_with(|| lua.create_string(s.clone().as_bytes()).unwrap());
        Some(spur)
    }

    pub fn dual_register_string(&mut self, lua: &Lua, s: &str) -> Option<Spur> {
        self.register_lua_string(lua, &lua.create_string(s).unwrap())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LuauFieldType {
    Bool,
    Integer,
    Number,
    Vector4,
    String,
    Buffer(usize),
}

impl LuauFieldType {
    pub fn layout(self) -> Layout {
        match self {
            Self::Bool => Layout::new::<bool>(),
            Self::Integer => Layout::new::<i64>(),
            Self::Number => Layout::new::<f64>(),
            Self::Vector4 => Layout::new::<[f32; 4]>(),
            Self::String => Layout::new::<Spur>(),
            Self::Buffer(n) => Layout::array::<u8>(n).unwrap(),
        }
    }
}

#[derive(Debug)]
pub struct DynamicComponentSchema {
    pub name: String,
    pub fields: HashMap<Spur, (usize, LuauFieldType)>,
    pub layout: Layout,
}

#[derive(Resource, Default)]
pub struct SchemaRegistry {
    pub name_to_id: HashMap<String, ComponentId>,
    pub id_to_schema: HashMap<ComponentId, DynamicComponentSchema>,
}

fn align_up(offset: usize, align: usize) -> usize {
    (offset + align - 1) & !(align - 1)
}

impl SchemaRegistry {
    pub fn register(
        &mut self,
        world: &mut World,
        name: String,
        fields: &[(Spur, LuauFieldType)],
    ) -> ComponentId {
        let mut offset = 0usize;
        let mut field_offsets = HashMap::new();

        for (spur, field_type) in fields {
            let layout = field_type.layout();

            offset = align_up(offset, layout.align());
            field_offsets.insert(*spur, (offset, *field_type));
            offset += layout.size();
        }

        let struct_align = fields
            .iter()
            .map(|(_, t)| t.layout().align())
            .max()
            .unwrap_or(1);

        let total_size = align_up(offset, struct_align);

        let layout =
            Layout::from_size_align(total_size, struct_align).expect("invalid dynamic layout");

        let schema = DynamicComponentSchema {
            name: name.clone(),
            fields: field_offsets,
            layout,
        };

        let descriptor = unsafe {
            ComponentDescriptor::new_with_layout(
                name.clone(),
                StorageType::Table,
                layout,
                None,
                true,
                ComponentCloneBehavior::Ignore,
                None,
            )
        };

        let id = world.register_component_with_descriptor(descriptor);

        self.name_to_id.insert(name, id);
        self.id_to_schema.insert(id, schema);

        id
    }
}

pub struct DynamicComponentBridge;

impl DynamicComponentBridge {
    /// # Safety
    /// yolo
    pub unsafe fn insert_from_table(
        world: &mut World,
        entity: Entity,
        component_id: ComponentId,
        registry: &SchemaRegistry,
        pool: &mut EngineStringPool,
        table: &LuaTable,
        lua: &Lua,
    ) -> LuaResult<()> {
        let schema = registry
            .id_to_schema
            .get(&component_id)
            .expect("Schema not registered");

        let layout = schema.layout;
        let scratch_ptr = unsafe { alloc_zeroed(layout) };
        if scratch_ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }

        for (&spur, &(offset, field_type)) in &schema.fields {
            let lua_string = pool.get_lua_str(spur);
            let field_ptr = unsafe { scratch_ptr.add(offset) };

            match (table.raw_get::<LuaValue>(lua_string)?, field_type) {
                (LuaValue::Boolean(b), LuauFieldType::Bool) => unsafe {
                    std::ptr::write(field_ptr.cast::<bool>(), b)
                },
                (LuaValue::Integer(i), LuauFieldType::Integer) => unsafe {
                    std::ptr::write(field_ptr.cast::<i64>(), i)
                },
                (LuaValue::Number(n), LuauFieldType::Number) => unsafe {
                    std::ptr::write(field_ptr.cast::<f64>(), n)
                },
                (LuaValue::Vector(v), LuauFieldType::Vector4) => unsafe {
                    std::ptr::write(field_ptr.cast::<[f32; 4]>(), [v.x(), v.y(), v.z(), v.w()])
                },
                (LuaValue::String(s), LuauFieldType::String) => {
                    if let Some(str_spur) = pool.register_lua_string(lua, &s) {
                        unsafe { std::ptr::write(field_ptr.cast::<Spur>(), str_spur) };
                    }
                }
                (LuaValue::Buffer(b), LuauFieldType::Buffer(len)) => {
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            b.to_pointer().cast::<u8>(),
                            field_ptr,
                            b.len().min(len),
                        )
                    };
                }
                _ => {}
            }
        }

        let non_null = unsafe { NonNull::new_unchecked(scratch_ptr) };
        let owning_ptr = unsafe { OwningPtr::new(non_null) };

        unsafe {
            world
                .entity_mut(entity)
                .insert_by_id(component_id, owning_ptr)
        };

        unsafe {
            dealloc(scratch_ptr, layout);
        }

        Ok(())
    }

    /// # Safety
    /// yolo
    pub unsafe fn extract_to_table(
        world: &World,
        entity: Entity,
        component_id: ComponentId,
        registry: &SchemaRegistry,
        pool: &EngineStringPool,
        lua: &Lua,
    ) -> LuaResult<Option<LuaTable>> {
        let Some(schema) = registry.id_to_schema.get(&component_id) else {
            return Ok(None);
        };
        let Ok(ptr) = world.entity(entity).get_by_id(component_id) else {
            return Ok(None);
        };

        let raw_ptr = ptr.as_ptr();
        let table = lua.create_table()?;

        for (&spur, &(offset, field_type)) in &schema.fields {
            let lua_string = pool.get_lua_str(spur);
            let field_ptr = unsafe { raw_ptr.add(offset) };

            match field_type {
                LuauFieldType::Bool => {
                    let val = unsafe { &*field_ptr.cast::<bool>() };
                    table.raw_set(lua_string, *val)?;
                }
                LuauFieldType::Integer => {
                    let val = unsafe { &*field_ptr.cast::<i64>() };
                    table.raw_set(lua_string, *val)?;
                }
                LuauFieldType::Number => {
                    let val = unsafe { &*field_ptr.cast::<f64>() };
                    table.raw_set(lua_string, *val)?;
                }
                LuauFieldType::Vector4 => {
                    let array_ref = unsafe { &*field_ptr.cast::<[f32; 4]>() };
                    table.raw_set(
                        lua_string,
                        mluau::Vector::new(array_ref[0], array_ref[1], array_ref[2], array_ref[3]),
                    )?;
                }
                LuauFieldType::String => {
                    let str_spur = unsafe { &*field_ptr.cast::<Spur>() };
                    table.raw_set(lua_string, pool.get_lua_str(*str_spur))?;
                }
                LuauFieldType::Buffer(len) => {
                    let slice = unsafe { std::slice::from_raw_parts(field_ptr, len) };
                    table.raw_set(lua_string, lua.create_buffer(slice)?)?;
                }
            }
        }

        Ok(Some(table))
    }
}

#[cfg(test)]
mod tests {
    use std::f64;

    use super::*;

    #[test]
    fn test_insert_extract_roundtrip() {
        let lua = Lua::new();
        let mut world = World::new();

        let mut pool = EngineStringPool {
            rodeo: Rodeo::new(),
            bridge: HashMap::new(),
        };

        let mut registry = SchemaRegistry::default();

        let fields = vec![
            (
                pool.dual_register_string(&lua, "a").unwrap(),
                LuauFieldType::Integer,
            ),
            (
                pool.dual_register_string(&lua, "b").unwrap(),
                LuauFieldType::Number,
            ),
        ];

        let id = registry.register(&mut world, "Test".into(), &fields);

        let entity = world.spawn_empty().id();

        let table = lua.create_table().unwrap();
        table.set("a", 42i64).unwrap();
        table.set("b", f64::consts::PI).unwrap();

        unsafe {
            DynamicComponentBridge::insert_from_table(
                &mut world, entity, id, &registry, &mut pool, &table, &lua,
            )
            .unwrap();

            let out = DynamicComponentBridge::extract_to_table(
                &world, entity, id, &registry, &pool, &lua,
            )
            .unwrap()
            .unwrap();

            assert_eq!(out.get::<i64>("a").unwrap(), 42);
            assert!((out.get::<f64>("b").unwrap() - f64::consts::PI).abs() < 1e-6);
        }
    }
}
