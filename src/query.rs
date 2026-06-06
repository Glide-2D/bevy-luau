use bevy::{
    ecs::{query::QueryBuilder, world::FilteredEntityMut},
    prelude::*,
};
use mluau::prelude::*;

use crate::bridge::DynamicComponentBridge;
use crate::pool::EngineStringPool;
use crate::runtime::ResolvedQuery;
use crate::schema::SchemaRegistry;

pub struct LuaTime {
    pub delta_secs: f64,
    pub elapsed_secs: f64,
}

impl LuaUserData for LuaTime {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(LuaMetaMethod::Index, |lua, this, key: LuaString| match key
            .to_str()?
            .as_ref()
        {
            "dt" => {
                let dt = this.delta_secs;
                Ok(LuaValue::Function(
                    lua.create_function(move |_, ()| Ok(dt))?,
                ))
            }
            "elapsed" => {
                let elapsed = this.elapsed_secs;
                Ok(LuaValue::Function(
                    lua.create_function(move |_, ()| Ok(elapsed))?,
                ))
            }
            _ => Ok(LuaValue::Nil),
        });
    }
}

#[derive(Clone)]
pub struct SnapshotRow {
    pub entity: Entity,
    pub mutable_tables: Vec<LuaTable>,
    pub immutable_tables: Vec<LuaTable>,
}

pub struct QuerySnapshot {
    pub desc: ResolvedQuery,
    pub rows: Vec<SnapshotRow>,
}

impl LuaUserData for QuerySnapshot {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("get", |_, this, entity_bits: i64| {
            let entity = Entity::from_bits(entity_bits as u64);
            match this.rows.iter().find(|r| r.entity == entity) {
                Some(row) => {
                    let vals: Vec<LuaValue> = row
                        .mutable_tables
                        .iter()
                        .map(|t| LuaValue::Table(t.clone()))
                        .collect();
                    Ok(LuaMultiValue::from_vec(vals))
                }
                None => Ok(LuaMultiValue::new()),
            }
        });

        methods.add_meta_method(LuaMetaMethod::Iter, |lua, this, ()| {
            let rows = this.rows.clone();
            let mut index = 0usize;
            lua.create_function_mut(move |_, ()| {
                if index >= rows.len() {
                    return Ok(LuaMultiValue::new());
                }
                let row = &rows[index];
                index += 1;
                let mut vals = vec![LuaValue::Integer(row.entity.to_bits() as i64)];
                for t in &row.mutable_tables {
                    vals.push(LuaValue::Table(t.clone()));
                }
                for t in &row.immutable_tables {
                    vals.push(LuaValue::Table(t.clone()));
                }
                Ok(LuaMultiValue::from_vec(vals))
            })
        });
    }
}

pub fn snapshot_query(
    world: &mut World,
    pool: &EngineStringPool,
    registry: &SchemaRegistry,
    lua: &Lua,
    desc: &ResolvedQuery,
) -> LuaResult<QuerySnapshot> {
    let mut builder = QueryBuilder::<FilteredEntityMut>::new(world);
    for &id in &desc.mutable {
        builder.mut_id(id);
    }
    for &id in &desc.immutable {
        builder.ref_id(id);
    }
    for &id in &desc.with {
        builder.with_id(id);
    }
    for &id in &desc.without {
        builder.without_id(id);
    }

    let mut state = builder.build();
    let entities: Vec<Entity> = state.iter_mut(world).map(|e| e.id()).collect();
    drop(state);

    let mut rows = Vec::with_capacity(entities.len());
    for entity in entities {
        let mut mutable_tables = Vec::new();
        let mut immutable_tables = Vec::new();

        for &comp_id in &desc.mutable {
            if let Some(t) = unsafe {
                DynamicComponentBridge::extract_to_table(
                    world, entity, comp_id, registry, pool, lua,
                )?
            } {
                mutable_tables.push(t);
            }
        }
        for &comp_id in &desc.immutable {
            if let Some(t) = unsafe {
                DynamicComponentBridge::extract_to_table(
                    world, entity, comp_id, registry, pool, lua,
                )?
            } {
                immutable_tables.push(t);
            }
        }

        rows.push(SnapshotRow {
            entity,
            mutable_tables,
            immutable_tables,
        });
    }

    Ok(QuerySnapshot {
        desc: desc.clone(),
        rows,
    })
}

pub fn writeback_snapshot(
    world: &mut World,
    pool: &mut EngineStringPool,
    registry: &SchemaRegistry,
    lua: &Lua,
    snapshot: &QuerySnapshot,
) -> LuaResult<()> {
    for row in &snapshot.rows {
        for (comp_id, table) in snapshot.desc.mutable.iter().zip(&row.mutable_tables) {
            unsafe {
                DynamicComponentBridge::insert_from_table(
                    world, row.entity, *comp_id, registry, pool, table, lua,
                )?;
            }
        }
    }
    Ok(())
}
