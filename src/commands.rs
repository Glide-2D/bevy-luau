use bevy::{ecs::component::ComponentId, prelude::*};
use mluau::prelude::*;

use crate::loading::LuaComponentMarker;

pub struct SpawnCmd {
    pub components: Vec<(ComponentId, Option<LuaTable>)>,
}

pub struct TriggerCmd {
    pub entity: Entity,
    pub event_id: ComponentId,
    pub data_table: LuaTable,
}

#[derive(Default)]
pub struct CommandBuffer {
    pub spawns: Vec<SpawnCmd>,
    pub despawns: Vec<Entity>,
    pub triggers: Vec<TriggerCmd>,
}

pub struct LuaCommandsHandle(pub *mut CommandBuffer);

impl LuaUserData for LuaCommandsHandle {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("Spawn", |_, this, components: LuaTable| {
            let buffer = unsafe { &mut *this.0 };
            let mut spawn = SpawnCmd {
                components: Vec::new(),
            };
            for pair in components.pairs::<LuaValue, LuaValue>() {
                let (key, value) = pair?;
                let comp_id = match key {
                    LuaValue::UserData(ref ud) => match ud.borrow::<LuaComponentMarker>() {
                        Ok(marker) => marker.component_id()?,
                        Err(_) => continue,
                    },
                    _ => continue,
                };
                let data = match value {
                    LuaValue::UserData(ud) if ud.is::<crate::loading::DefaultMarker>() => None,
                    LuaValue::Table(t) => Some(t),
                    _ => None,
                };
                spawn.components.push((comp_id, data));
            }
            buffer.spawns.push(spawn);
            Ok(())
        });

        methods.add_method("Despawn", |_, this, entity_bits: i64| {
            let buffer = unsafe { &mut *this.0 };
            buffer.despawns.push(Entity::from_bits(entity_bits as u64));
            Ok(())
        });

        methods.add_method(
            "Trigger",
            |_, this, (entity_bits, event_ud, data): (i64, LuaAnyUserData, LuaTable)| {
                let buffer = unsafe { &mut *this.0 };
                let entity = Entity::from_bits(entity_bits as u64);
                let event_id = event_ud.borrow::<LuaComponentMarker>()?.component_id()?;
                buffer.triggers.push(TriggerCmd {
                    entity,
                    event_id,
                    data_table: data,
                });
                Ok(())
            },
        );
    }
}
