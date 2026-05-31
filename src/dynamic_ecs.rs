use bevy::{
    ecs::component::{
        ComponentCloneBehavior, ComponentDescriptor, ComponentMutability, Mutable, StorageType,
    },
    prelude::*,
};
use mluau::prelude::*;
use std::{
    alloc::Layout,
    collections::BTreeMap,
    ffi::{c_double, c_long},
};

#[derive(Debug, Clone, Copy)]
pub enum FieldType {
    Bool,
    Int,
    Float,
}

impl FieldType {
    pub fn layout(&self) -> Layout {
        match self {
            FieldType::Bool => Layout::new::<bool>(),
            FieldType::Int => Layout::new::<c_long>(),
            FieldType::Float => Layout::new::<c_double>(),
        }
    }
}

impl TryFrom<LuaValue> for FieldType {
    type Error = &'static str;
    fn try_from(value: LuaValue) -> Result<Self, Self::Error> {
        match value {
            LuaValue::Boolean(_) => Ok(Self::Bool),
            LuaValue::Integer(_) => Ok(Self::Int),
            LuaValue::Number(_) => Ok(Self::Float),
            _ => Err("err"),
        }
    }
}

pub fn register_component(world: &mut World, value: LuaTable) {
    let fields: BTreeMap<String, LuaValue> = value
        .pairs::<String, LuaValue>()
        .map(|res| res.unwrap())
        .collect();

    info!("{fields:?}");

    let mut layout = Layout::new::<()>();

    for (_, field_type) in fields {
        let (new_layout, _offset) = layout
            .extend(FieldType::try_from(field_type).unwrap().layout())
            .unwrap();
        layout = new_layout;
    }

    info!("{layout:?}");

    world.register_component_with_descriptor(unsafe {
        ComponentDescriptor::new_with_layout(
            "lua_table",
            StorageType::Table,
            layout,
            None,
            Mutable::MUTABLE,
            ComponentCloneBehavior::Default,
            None,
        )
    });
}

pub fn test_register_component_extracts_fields() {
    let mut world = World::new();

    let lua = Lua::new();

    let table = lua.create_table().unwrap();

    table.set("x", 10.0f32).unwrap();
    table.set("y", 20.0f32).unwrap();
    table.set("is_active", true).unwrap();

    register_component(&mut world, table);
}
