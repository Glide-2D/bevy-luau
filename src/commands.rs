use bevy::{ecs::component::ComponentId, prelude::*};
use mluau::prelude::*;
use smallvec::SmallVec;

use crate::loading::LuaComponentMarker;

pub struct SpawnCmd {
    pub components: SmallVec<[(ComponentId, Option<LuaTable>); 8]>,
}

pub struct TriggerCmd {
    pub entity: Entity,
    pub event_id: ComponentId,
    pub data_table: LuaTable,
}

#[derive(Default)]
pub struct CommandBuffer {
    pub spawns: SmallVec<[SpawnCmd; 4]>,
    pub despawns: SmallVec<[Entity; 8]>,
    pub triggers: SmallVec<[TriggerCmd; 8]>,
}

pub struct LuaCommandsHandle(pub *mut CommandBuffer);

impl LuaUserData for LuaCommandsHandle {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("Spawn", |_, this, components: LuaTable| {
            let mut spawn = SpawnCmd {
                components: SmallVec::new(),
            };

            for pair in components.pairs::<LuaValue, LuaValue>() {
                let (key, value) = pair?;

                let LuaValue::UserData(ref ud) = key else {
                    return Err(LuaError::RuntimeError(
                        "Spawn keys must be Component markers".into(),
                    ));
                };

                let marker = ud.borrow::<LuaComponentMarker>()?;
                let comp_id = marker.component_id()?;

                let data = match value {
                    LuaValue::UserData(ud) if ud.is::<crate::loading::DefaultMarker>() => None,
                    LuaValue::Table(t) => Some(t),
                    _ => {
                        return Err(LuaError::RuntimeError(
                            "Component data must be a table or DefaultMarker".into(),
                        ));
                    }
                };

                spawn.components.push((comp_id, data));
            }

            unsafe { (*this.0).spawns.push(spawn) };

            Ok(())
        });

        methods.add_method("Despawn", |_, this, entity_bits: i64| {
            let entity = Entity::from_bits(entity_bits.cast_unsigned());
            unsafe { (*this.0).despawns.push(entity) };
            Ok(())
        });

        methods.add_method(
            "Trigger",
            |_, this, (entity_bits, event_ud, data): (i64, LuaAnyUserData, LuaTable)| {
                let entity = Entity::from_bits(entity_bits.cast_unsigned());
                let event_id = event_ud.borrow::<LuaComponentMarker>()?.component_id()?;

                unsafe {
                    (*this.0).triggers.push(TriggerCmd {
                        entity,
                        event_id,
                        data_table: data,
                    });
                }
                Ok(())
            },
        );
    }
}
