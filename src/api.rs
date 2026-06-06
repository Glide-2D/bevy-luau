use bevy::ecs::component::ComponentId;
use bevy::prelude::*;
use mluau::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Clone, Copy)]
pub enum LuaSchedule {
    Startup,
    Update,
}

#[derive(Clone, Default)]
pub struct LuaQuery {
    pub mutable: Vec<ComponentId>,
    pub immutable: Vec<ComponentId>,
    pub with: Vec<ComponentId>,
    pub without: Vec<ComponentId>,
}

#[derive(Clone)]
pub enum LuaParam {
    Commands,
    Time,
    Query(LuaQuery),
    Resource(ComponentId),
}

pub struct LuaSystemDescriptor {
    pub func: LuaFunction,
    pub schedule: LuaSchedule,
    pub params: Vec<LuaParam>,
}

pub struct LuaObserverDescriptor {
    pub event_id: ComponentId,
    pub func: LuaFunction,
    pub params: Vec<LuaParam>,
}

#[derive(Clone, Copy)]
pub struct LuaComponentMarker(pub ComponentId);
#[derive(Clone, Copy)]
pub struct LuaResourceMarker(pub ComponentId);
#[derive(Clone, Copy)]
pub struct ScheduleMarker(pub LuaSchedule);
#[derive(Clone)]
pub struct QueryDescHandle(pub LuaQuery);

pub struct CommandsParam;
pub struct TimeParam;
pub struct DefaultMarker;

impl LuaUserData for LuaComponentMarker {}
impl LuaUserData for LuaResourceMarker {}
impl LuaUserData for ScheduleMarker {}
impl LuaUserData for QueryDescHandle {}
impl LuaUserData for CommandsParam {}
impl LuaUserData for TimeParam {}
impl LuaUserData for DefaultMarker {}

pub struct LuaTime {
    pub delta_secs: f64,
    pub elapsed_secs: f64,
}

impl LuaUserData for LuaTime {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("dt", |_, this, ()| Ok(this.delta_secs));
        methods.add_method("elapsed", |_, this, ()| Ok(this.elapsed_secs));
    }
}

#[derive(Default)]
pub struct CommandBuffer {
    pub spawns: Vec<SpawnCmd>,
    pub despawns: Vec<Entity>,
    pub triggers: Vec<TriggerCmd>,
}

pub struct SpawnCmd {
    pub components: Vec<(ComponentId, Option<LuaTable>)>,
}
pub struct TriggerCmd {
    pub entity: Entity,
    pub event_id: ComponentId,
    pub data_table: LuaTable,
}

// Replaced mut with Rc<RefCell>
pub struct LuaCommandsHandle(pub Rc<RefCell<CommandBuffer>>);

impl LuaUserData for LuaCommandsHandle {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("Spawn", |_, this, components: LuaTable| {
            let mut spawn = SpawnCmd {
                components: Vec::new(),
            };

            for pair in components.pairs::<LuaValue, LuaValue>() {
                let (key, value) = pair?;
                if let LuaValue::UserData(ud) = key {
                    if let Ok(marker) = ud.borrow::<LuaComponentMarker>() {
                        let data = if let LuaValue::Table(t) = value {
                            Some(t)
                        } else {
                            None
                        };
                        spawn.components.push((marker.0, data));
                    }
                }
            }
            this.0.borrow_mut().spawns.push(spawn);
            Ok(())
        });

        methods.add_method("Despawn", |_, this, entity_bits: i64| {
            this.0
                .borrow_mut()
                .despawns
                .push(Entity::from_bits(entity_bits as u64));
            Ok(())
        });

        methods.add_method(
            "Trigger",
            |_, this, (entity_bits, event_ud, data): (i64, LuaAnyUserData, LuaTable)| {
                let entity = Entity::from_bits(entity_bits as u64);
                let event_id = event_ud.borrow::<LuaComponentMarker>()?.0;

                this.0.borrow_mut().triggers.push(TriggerCmd {
                    entity,
                    event_id,
                    data_table: data,
                });
                Ok(())
            },
        );
    }
}

pub struct EcsHandle;

impl LuaUserData for EcsHandle {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("Startup", |lua, _, ()| {
            lua.create_userdata(ScheduleMarker(LuaSchedule::Startup))
        });
        methods.add_method("Update", |lua, _, ()| {
            lua.create_userdata(ScheduleMarker(LuaSchedule::Update))
        });
        methods.add_method("Commands", |lua, _, ()| lua.create_userdata(CommandsParam));
        methods.add_method("Time", |lua, _, ()| lua.create_userdata(TimeParam));
        methods.add_method("Default", |lua, _, ()| lua.create_userdata(DefaultMarker));

        methods.add_method("RegisterComponent", |lua, _, schema_table: LuaTable| {
            let id = crate::systems::register_schema_immediate(lua, &schema_table, false)?;
            lua.create_userdata(LuaComponentMarker(id))
        });

        methods.add_method("RegisterEvent", |lua, _, schema_table: LuaTable| {
            let id = crate::systems::register_schema_immediate(lua, &schema_table, false)?;
            lua.create_userdata(LuaComponentMarker(id))
        });

        methods.add_method("RegisterResource", |lua, _, schema_table: LuaTable| {
            let id = crate::systems::register_schema_immediate(lua, &schema_table, true)?;
            lua.create_userdata(LuaResourceMarker(id))
        });

        methods.add_method("Query", |lua, _, def: LuaTable| {
            let read_ids = |key: &str| -> LuaResult<Vec<ComponentId>> {
                match def.get::<Option<LuaTable>>(key)? {
                    Some(t) => t
                        .sequence_values::<LuaAnyUserData>()
                        .map(|v| Ok(v?.borrow::<LuaComponentMarker>()?.0))
                        .collect(),
                    None => Ok(Vec::new()),
                }
            };

            lua.create_userdata(QueryDescHandle(LuaQuery {
                mutable: read_ids("Mutable")?,
                immutable: read_ids("Immutable")?,
                with: read_ids("With")?,
                without: read_ids("Without")?,
            }))
        });

        methods.add_method(
            "RegisterSystem",
            |lua, _, (func, sched_ud, params_tbl): (LuaFunction, LuaAnyUserData, LuaTable)| {
                let schedule = sched_ud.borrow::<ScheduleMarker>()?.0;
                let params = parse_lua_params(&params_tbl)?;

                crate::systems::with_ctx(lua, |ctx| {
                    // Removed unsafe raw pointer mutation.
                    ctx.runtime.borrow_mut().systems.push(LuaSystemDescriptor {
                        func,
                        schedule,
                        params,
                    });
                    Ok(())
                })
            },
        );

        methods.add_method(
            "Observe",
            |lua, _, (event_ud, func, params_tbl): (LuaAnyUserData, LuaFunction, LuaTable)| {
                let event_id = event_ud.borrow::<LuaComponentMarker>()?.0;
                let params = parse_lua_params(&params_tbl)?;

                crate::systems::with_ctx(lua, |ctx| {
                    // Removed unsafe raw pointer mutation.
                    ctx.runtime
                        .borrow_mut()
                        .observers
                        .push(LuaObserverDescriptor {
                            event_id,
                            func,
                            params,
                        });
                    Ok(())
                })
            },
        );
    }
}

// Simplified
pub fn parse_lua_params(table: &LuaTable) -> LuaResult<Vec<LuaParam>> {
    table
        .sequence_values::<LuaAnyUserData>()
        .map(|ud| {
            let ud = ud?;
            if ud.is::<CommandsParam>() {
                Ok(LuaParam::Commands)
            } else if ud.is::<TimeParam>() {
                Ok(LuaParam::Time)
            } else if let Ok(q) = ud.borrow::<QueryDescHandle>() {
                Ok(LuaParam::Query(q.0.clone()))
            } else if let Ok(r) = ud.borrow::<LuaResourceMarker>() {
                Ok(LuaParam::Resource(r.0))
            } else {
                Err(LuaError::runtime("invalid param type"))
            }
        })
        .collect()
}

#[derive(Clone)]
pub struct SnapshotRow {
    pub entity: Entity,
    pub mutable_tables: Vec<LuaTable>,
    pub immutable_tables: Vec<LuaTable>,
}

pub struct QuerySnapshot {
    pub desc: LuaQuery,
    // Prevent memory bloat
    pub rows: Rc<Vec<SnapshotRow>>,
}

impl LuaUserData for QuerySnapshot {
    fn add_methods<M: LuaUserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("get", |_, this, entity_bits: i64| {
            let entity = Entity::from_bits(entity_bits as u64);
            if let Some(row) = this.rows.iter().find(|r| r.entity == entity) {
                let vals = row
                    .mutable_tables
                    .iter()
                    .map(|t| LuaValue::Table(t.clone()))
                    .collect();
                Ok(LuaMultiValue::from_vec(vals))
            } else {
                Ok(LuaMultiValue::new())
            }
        });

        methods.add_meta_method(LuaMetaMethod::Iter, |lua, this, ()| {
            let rows = Rc::clone(&this.rows);
            let mut index = 0usize;

            lua.create_function_mut(move |_, ()| {
                if index >= rows.len() {
                    return Ok(LuaMultiValue::new());
                }
                let row = &rows[index];
                index += 1;
                let mut vals = vec![LuaValue::Integer(row.entity.to_bits() as i64)];
                vals.extend(
                    row.mutable_tables
                        .iter()
                        .chain(&row.immutable_tables)
                        .map(|t| LuaValue::Table(t.clone())),
                );
                Ok(LuaMultiValue::from_vec(vals))
            })
        });
    }
}
