use bevy::prelude::*;
use mluau::prelude::*;

use crate::bridge::DynamicComponentBridge;
use crate::commands::{CommandBuffer, TriggerCmd};
use crate::pool::EngineStringPool;
use crate::query::{LuaTime, QuerySnapshot, snapshot_query, writeback_snapshot};
use crate::runtime::{LuaObserverDescriptor, LuaParam, LuaSystemDescriptor, ScriptingRuntime};
use crate::schema::{SchemaRegistry, extract_resource_table, writeback_resource_table};
use crate::types::LuaSchedule;

pub fn lua_startup_system(world: &mut World) {
    let runtime = world.remove_non_send::<ScriptingRuntime>().unwrap();
    let mut pool = world.remove_non_send::<EngineStringPool>().unwrap();

    let indices: Vec<usize> = (0..runtime.systems.len())
        .filter(|&i| matches!(runtime.systems[i].schedule, LuaSchedule::Startup))
        .collect();

    for i in indices {
        run_lua_system(
            world,
            &runtime.lua,
            &mut pool,
            &runtime.observers,
            &runtime.systems[i],
        );
    }

    world.insert_non_send(runtime);
    world.insert_non_send(pool);
}

pub fn lua_update_system(world: &mut World) {
    let runtime = world.remove_non_send::<ScriptingRuntime>().unwrap();
    let mut pool = world.remove_non_send::<EngineStringPool>().unwrap();

    let indices: Vec<usize> = (0..runtime.systems.len())
        .filter(|&i| matches!(runtime.systems[i].schedule, LuaSchedule::Update))
        .collect();

    for i in indices {
        run_lua_system(
            world,
            &runtime.lua,
            &mut pool,
            &runtime.observers,
            &runtime.systems[i],
        );
    }

    world.insert_non_send(runtime);
    world.insert_non_send(pool);
}

pub fn run_lua_system(
    world: &mut World,
    lua: &Lua,
    pool: &mut EngineStringPool,
    observers: &[LuaObserverDescriptor],
    system: &LuaSystemDescriptor,
) {
    let delta_secs = world.resource::<Time>().delta_secs() as f64;
    let elapsed_secs = world.resource::<Time>().elapsed().as_secs_f64();

    let mut cmd_buffer = CommandBuffer::default();
    let cmd_ptr = std::ptr::addr_of_mut!(cmd_buffer);

    world.resource_scope(|world, mut registry: Mut<SchemaRegistry>| {
        let mut args = Vec::<LuaValue>::new();

        for param in &system.params {
            args.push(match param {
                LuaParam::Commands => lua
                    .create_userdata(crate::commands::LuaCommandsHandle(cmd_ptr))
                    .map(LuaValue::UserData)
                    .unwrap(),
                LuaParam::Time => lua
                    .create_userdata(LuaTime {
                        delta_secs,
                        elapsed_secs,
                    })
                    .map(LuaValue::UserData)
                    .unwrap(),
                LuaParam::Query(desc) => {
                    let snap = snapshot_query(world, pool, &registry, lua, desc).unwrap();
                    lua.create_userdata(snap).map(LuaValue::UserData).unwrap()
                }
                LuaParam::Resource(id) => extract_resource_table(&registry, pool, lua, *id)
                    .unwrap()
                    .map(LuaValue::Table)
                    .unwrap_or_else(|| LuaValue::Table(lua.create_table().unwrap())),
            });
        }

        if let Err(e) = system
            .func
            .call::<LuaMultiValue>(LuaMultiValue::from_vec(args.clone()))
        {
            error!("{e}");
        }

        for (param, val) in system.params.iter().zip(args.iter()) {
            match (param, val) {
                (LuaParam::Query(_), LuaValue::UserData(ud)) => {
                    if let Ok(snap) = ud.borrow::<QuerySnapshot>() {
                        writeback_snapshot(world, pool, &registry, lua, &snap).ok();
                    }
                }
                (LuaParam::Resource(id), LuaValue::Table(t)) => {
                    writeback_resource_table(&mut registry, pool, *id, t).ok();
                }
                _ => {}
            }
        }
    });

    flush_commands(world, pool, lua, cmd_buffer, observers);
}

pub fn run_lua_observer(
    world: &mut World,
    pool: &mut EngineStringPool,
    lua: &Lua,
    observer: &LuaObserverDescriptor,
    entity: Entity,
    event_data: &LuaTable,
    observers: &[LuaObserverDescriptor],
) {
    let mut cmd_buffer = CommandBuffer::default();
    let cmd_ptr = std::ptr::addr_of_mut!(cmd_buffer);

    world.resource_scope(|world, registry: Mut<SchemaRegistry>| {
        let mut args = vec![
            LuaValue::Integer(entity.to_bits() as i64),
            LuaValue::Table(event_data.clone()),
        ];

        for param in &observer.params {
            args.push(match param {
                LuaParam::Commands => lua
                    .create_userdata(crate::commands::LuaCommandsHandle(cmd_ptr))
                    .map(LuaValue::UserData)
                    .unwrap(),
                LuaParam::Query(desc) => {
                    let snap = snapshot_query(world, pool, &registry, lua, desc).unwrap();
                    lua.create_userdata(snap).map(LuaValue::UserData).unwrap()
                }
                _ => LuaValue::Nil,
            });
        }

        if let Err(e) = observer
            .func
            .call::<LuaMultiValue>(LuaMultiValue::from_vec(args.clone()))
        {
            error!("{e}");
        }

        for (param, val) in observer.params.iter().zip(args[2..].iter()) {
            if let (LuaParam::Query(_), LuaValue::UserData(ud)) = (param, val)
                && let Ok(snap) = ud.borrow::<QuerySnapshot>()
            {
                writeback_snapshot(world, pool, &registry, lua, &snap).ok();
            }
        }
    });

    flush_commands(world, pool, lua, cmd_buffer, observers);
}

fn dispatch_trigger(
    world: &mut World,
    pool: &mut EngineStringPool,
    lua: &Lua,
    trigger: TriggerCmd,
    observers: &[LuaObserverDescriptor],
) {
    let indices: Vec<usize> = observers
        .iter()
        .enumerate()
        .filter(|(_, o)| o.event_id == trigger.event_id)
        .map(|(i, _)| i)
        .collect();

    for idx in indices {
        run_lua_observer(
            world,
            pool,
            lua,
            &observers[idx],
            trigger.entity,
            &trigger.data_table,
            observers,
        );
    }
}

fn flush_commands(
    world: &mut World,
    pool: &mut EngineStringPool,
    lua: &Lua,
    buffer: CommandBuffer,
    observers: &[LuaObserverDescriptor],
) {
    world.resource_scope(|world, registry: Mut<SchemaRegistry>| {
        for spawn in buffer.spawns {
            let entity = world.spawn_empty().id();
            for (comp_id, data) in spawn.components {
                match data {
                    Some(ref table) => unsafe {
                        DynamicComponentBridge::insert_from_table(
                            world, entity, comp_id, &registry, pool, table, lua,
                        )
                        .ok();
                    },
                    None => unsafe {
                        DynamicComponentBridge::insert_default(world, entity, comp_id, &registry);
                    },
                }
            }
        }
    });

    for entity in buffer.despawns {
        world.despawn(entity);
    }

    for trigger in buffer.triggers {
        dispatch_trigger(world, pool, lua, trigger, observers);
    }
}
