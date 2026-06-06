use bevy::{ecs::component::ComponentId, prelude::*, ptr::OwningPtr};
use lasso::Spur;
use mluau::prelude::*;
use std::{
    alloc::{Layout, alloc_zeroed, dealloc},
    ptr::NonNull,
};

use crate::pool::EngineStringPool;
use crate::schema::SchemaRegistry;
use crate::types::LuauFieldType;

struct ScratchGuard {
    ptr: *mut u8,
    layout: Layout,
}

impl ScratchGuard {
    fn new(layout: Layout) -> Self {
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        Self { ptr, layout }
    }
}

impl Drop for ScratchGuard {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, self.layout) };
    }
}

pub struct DynamicComponentBridge;

impl DynamicComponentBridge {
    /// # Safety
    /// # Panics
    /// # Errors
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
            .expect("schema not registered");

        let guard = ScratchGuard::new(schema.layout);

        for (&_, &(offset, ft)) in &schema.fields {
            if ft == LuauFieldType::String {
                unsafe {
                    guard
                        .ptr
                        .add(offset)
                        .cast::<Spur>()
                        .write_unaligned(Spur::default());
                }
            }
        }

        for (&spur, &(offset, ft)) in &schema.fields {
            let lua_str = pool.get_lua_str(spur);
            let field_ptr = unsafe { guard.ptr.add(offset) };

            match (table.raw_get::<LuaValue>(lua_str)?, ft) {
                (LuaValue::Boolean(b), LuauFieldType::Bool) => unsafe {
                    std::ptr::write(field_ptr.cast::<bool>(), b);
                },
                (LuaValue::Integer(i), LuauFieldType::Integer) => unsafe {
                    field_ptr.cast::<i64>().write_unaligned(i);
                },
                (LuaValue::Number(n), LuauFieldType::Number) => unsafe {
                    field_ptr.cast::<f64>().write_unaligned(n);
                },
                (LuaValue::Vector(v), LuauFieldType::Vector4) => unsafe {
                    field_ptr
                        .cast::<[f32; 4]>()
                        .write_unaligned([v.x(), v.y(), v.z(), v.w()]);
                },
                (LuaValue::String(s), LuauFieldType::String) => {
                    let sp = pool.intern(lua, s.to_str()?.as_ref());
                    unsafe { field_ptr.cast::<Spur>().write_unaligned(sp) };
                }
                (LuaValue::Buffer(b), LuauFieldType::Buffer(len)) => unsafe {
                    std::ptr::copy_nonoverlapping(
                        b.to_pointer().cast::<u8>(),
                        field_ptr,
                        b.len().min(len),
                    );
                },
                _ => {}
            }
        }

        let owning = unsafe { OwningPtr::new(NonNull::new_unchecked(guard.ptr)) };
        unsafe { world.entity_mut(entity).insert_by_id(component_id, owning) };
        Ok(())
    }

    /// # Safety
    /// # Panics
    pub unsafe fn insert_default(
        world: &mut World,
        entity: Entity,
        component_id: ComponentId,
        registry: &SchemaRegistry,
    ) {
        let schema = registry
            .id_to_schema
            .get(&component_id)
            .expect("schema not registered");

        let guard = ScratchGuard::new(schema.layout);
        for (&_, &(offset, ft)) in &schema.fields {
            if ft == LuauFieldType::String {
                unsafe {
                    guard
                        .ptr
                        .add(offset)
                        .cast::<Spur>()
                        .write_unaligned(Spur::default());
                }
            }
        }

        let owning = unsafe { OwningPtr::new(NonNull::new_unchecked(guard.ptr)) };
        unsafe { world.entity_mut(entity).insert_by_id(component_id, owning) };
    }

    /// # Safety
    /// # Errors
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

        let raw = ptr.as_ptr();
        let table = lua.create_table()?;

        for (&spur, &(offset, ft)) in &schema.fields {
            let lua_str = pool.get_lua_str(spur);
            let field_ptr = unsafe { raw.add(offset) };
            match ft {
                LuauFieldType::Bool => {
                    table.raw_set(lua_str, unsafe { *field_ptr.cast::<bool>() })?;
                }
                LuauFieldType::Integer => {
                    table.raw_set(lua_str, unsafe { field_ptr.cast::<i64>().read_unaligned() })?;
                }
                LuauFieldType::Number => {
                    table.raw_set(lua_str, unsafe { field_ptr.cast::<f64>().read_unaligned() })?;
                }
                LuauFieldType::Vector4 => {
                    let v = unsafe { field_ptr.cast::<[f32; 4]>().read_unaligned() };
                    table.raw_set(lua_str, mluau::Vector::new(v[0], v[1], v[2], v[3]))?;
                }
                LuauFieldType::String => {
                    let sp = unsafe { field_ptr.cast::<Spur>().read_unaligned() };
                    table.raw_set(lua_str, pool.get_lua_str(sp))?;
                }
                LuauFieldType::Buffer(len) => {
                    let slice = unsafe { std::slice::from_raw_parts(field_ptr, len) };
                    table.raw_set(lua_str, lua.create_buffer(slice)?)?;
                }
            }
        }

        Ok(Some(table))
    }
}
