use bevy::{ecs::component::ComponentId, prelude::*};
use mluau::prelude::*;
use smallvec::SmallVec;

use crate::pool::EngineStringPool;
use crate::runtime::{
    LuaObserverDescriptor, LuaParam, LuaSystemDescriptor, LuauResolver, ResolvedQuery,
    ScriptingRuntime,
};
use crate::schema::SchemaRegistry;
use crate::types::{LuaSchedule, LuauFieldType};

pub struct LuaComponentMarker {
    pub staging_idx: usize,
    pub resolved_id: Option<ComponentId>,
}

impl LuaComponentMarker {
    /// # Errors
    pub fn component_id(&self) -> LuaResult<ComponentId> {
        self.resolved_id
            .ok_or_else(|| LuaError::runtime("component marker not yet resolved"))
    }
}

impl LuaUserData for LuaComponentMarker {}

#[derive(Clone, Copy)]
pub struct ScheduleMarker(pub LuaSchedule);
impl LuaUserData for ScheduleMarker {}

pub struct CommandsParam;
pub struct TimeParam;
pub struct DefaultMarker;

#[derive(Clone, Copy)]
pub struct ResourceDesc(pub usize);

impl LuaUserData for CommandsParam {}
impl LuaUserData for TimeParam {}
impl LuaUserData for DefaultMarker {}
impl LuaUserData for ResourceDesc {}

#[derive(Clone, Default)]
struct StagedQuery {
    mutable: SmallVec<[usize; 4]>,
    immutable: SmallVec<[usize; 4]>,
    with: SmallVec<[usize; 4]>,
    without: SmallVec<[usize; 4]>,
}

#[derive(Clone)]
enum StagedParam {
    Commands,
    Time,
    Query(StagedQuery),
    Resource(usize),
}

pub(crate) struct StagedSystem {
    func: LuaFunction,
    schedule: LuaSchedule,
    params: SmallVec<[StagedParam; 6]>,
}

pub(crate) struct StagedObserver {
    event_idx: usize,
    func: LuaFunction,
    params: SmallVec<[StagedParam; 6]>,
}

pub(crate) struct ComponentBlueprint {
    pub name: String,
    pub fields: Vec<(lasso::Spur, LuauFieldType)>,
    pub is_resource: bool,
}

struct QueryDescHandle(StagedQuery);
impl LuaUserData for QueryDescHandle {}

pub(crate) struct LoadContext {
    pub pool: *mut EngineStringPool,
    pub pending_components: Vec<ComponentBlueprint>,
    pub pending_systems: Vec<StagedSystem>,
    pub pending_observers: Vec<StagedObserver>,
    pub component_markers: Vec<LuaAnyUserData>,
}

pub(crate) struct ScriptLoadCtx(pub *mut LoadContext);

pub(crate) fn with_ctx<T>(
    lua: &Lua,
    f: impl FnOnce(&mut LoadContext) -> LuaResult<T>,
) -> LuaResult<T> {
    let ptr = {
        let guard = lua
            .app_data_ref::<ScriptLoadCtx>()
            .ok_or_else(|| LuaError::runtime("Ecs API only available during script loading"))?;
        guard.0
    };
    f(unsafe { &mut *ptr })
}

struct EcsHandle;

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
            register_schema(lua, &schema_table, false)
        });

        methods.add_method("RegisterEvent", |lua, _, schema_table: LuaTable| {
            register_schema(lua, &schema_table, false)
        });

        methods.add_method("RegisterResource", |lua, _, schema_table: LuaTable| {
            let marker_ud = register_schema(lua, &schema_table, true)?;
            let idx = marker_ud.borrow::<LuaComponentMarker>()?.staging_idx;
            lua.create_userdata(ResourceDesc(idx))
        });

        methods.add_method("Query", |lua, _, def: LuaTable| {
            let read_staging_ids = |key: &str| -> LuaResult<SmallVec<[usize; 4]>> {
                let t: Option<LuaTable> = def.get(key)?;
                t.map_or_else(
                    || Ok(SmallVec::new()),
                    |t| {
                        t.sequence_values::<LuaAnyUserData>()
                            .map(|v| Ok(v?.borrow::<LuaComponentMarker>()?.staging_idx))
                            .collect()
                    },
                )
            };
            lua.create_userdata(QueryDescHandle(StagedQuery {
                mutable: read_staging_ids("Mutable")?,
                immutable: read_staging_ids("Immutable")?,
                with: read_staging_ids("With")?,
                without: read_staging_ids("Without")?,
            }))
        });

        methods.add_method(
            "RegisterSystem",
            |lua, _, (func, sched_ud, params_tbl): (LuaFunction, LuaAnyUserData, LuaTable)| {
                let schedule = sched_ud.borrow::<ScheduleMarker>()?.0;
                let params = parse_staged_params(&params_tbl)?;
                with_ctx(lua, |ctx| {
                    ctx.pending_systems.push(StagedSystem {
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
                let event_idx = event_ud.borrow::<LuaComponentMarker>()?.staging_idx;
                let params = parse_staged_params(&params_tbl)?;
                with_ctx(lua, |ctx| {
                    ctx.pending_observers.push(StagedObserver {
                        event_idx,
                        func,
                        params,
                    });
                    Ok(())
                })
            },
        );
    }
}

fn register_schema(
    lua: &Lua,
    schema_table: &LuaTable,
    is_resource: bool,
) -> LuaResult<LuaAnyUserData> {
    with_ctx(lua, |ctx| {
        let pool = unsafe { &mut *ctx.pool };
        let fields = collect_fields(lua, pool, schema_table)?;
        let index = ctx.pending_components.len();
        let prefix = if is_resource {
            "__lua_res"
        } else {
            "__lua_comp"
        };
        ctx.pending_components.push(ComponentBlueprint {
            name: format!("{prefix}_{index}"),
            fields,
            is_resource,
        });
        let ud = lua.create_userdata(LuaComponentMarker {
            staging_idx: index,
            resolved_id: None,
        })?;
        ctx.component_markers.push(ud.clone());
        Ok(ud)
    })
}

fn collect_fields(
    lua: &Lua,
    pool: &mut EngineStringPool,
    table: &LuaTable,
) -> LuaResult<Vec<(lasso::Spur, LuauFieldType)>> {
    table
        .pairs::<LuaString, LuaValue>()
        .map(|pair| {
            let (key, value) = pair?;
            let ft = infer_field_type(&value)?;
            let spur = pool.intern(lua, key.to_str()?.as_ref());
            Ok((spur, ft))
        })
        .collect()
}

fn infer_field_type(value: &LuaValue) -> LuaResult<LuauFieldType> {
    match value {
        LuaValue::Boolean(_) => Ok(LuauFieldType::Bool),
        LuaValue::Integer(_) => Ok(LuauFieldType::Integer),
        LuaValue::Number(_) => Ok(LuauFieldType::Number),
        LuaValue::Vector(_) => Ok(LuauFieldType::Vector4),
        LuaValue::String(_) => Ok(LuauFieldType::String),
        LuaValue::Buffer(b) => Ok(LuauFieldType::Buffer(b.len())),
        other => Err(LuaError::runtime(format!(
            "cannot infer field type from '{}'",
            other.type_name()
        ))),
    }
}

fn parse_staged_params(table: &LuaTable) -> LuaResult<SmallVec<[StagedParam; 6]>> {
    table
        .sequence_values::<LuaValue>()
        .map(|val| match val? {
            LuaValue::UserData(ud) if ud.is::<CommandsParam>() => Ok(StagedParam::Commands),
            LuaValue::UserData(ud) if ud.is::<TimeParam>() => Ok(StagedParam::Time),
            LuaValue::UserData(ud) if ud.is::<QueryDescHandle>() => Ok(StagedParam::Query(
                ud.borrow::<QueryDescHandle>()?.0.clone(),
            )),
            LuaValue::UserData(ud) if ud.is::<ResourceDesc>() => {
                Ok(StagedParam::Resource(ud.borrow::<ResourceDesc>()?.0))
            }
            other => Err(LuaError::runtime(format!(
                "invalid param type '{}'",
                other.type_name()
            ))),
        })
        .collect()
}

fn resolve_param(param: StagedParam, real_ids: &[ComponentId]) -> LuaParam {
    match param {
        StagedParam::Commands => LuaParam::Commands,
        StagedParam::Time => LuaParam::Time,
        StagedParam::Resource(idx) => LuaParam::Resource(real_ids[idx]),
        StagedParam::Query(q) => LuaParam::Query(ResolvedQuery {
            mutable: q.mutable.iter().map(|&i| real_ids[i]).collect(),
            immutable: q.immutable.iter().map(|&i| real_ids[i]).collect(),
            with: q.with.iter().map(|&i| real_ids[i]).collect(),
            without: q.without.iter().map(|&i| real_ids[i]).collect(),
        }),
    }
}

fn set_globals(lua: &Lua) {
    let globals = lua.globals();

    let require_fn = lua
        .create_require_function(LuauResolver::default())
        .unwrap();

    globals.set("require", require_fn).unwrap();

    globals
        .set(
            "print",
            lua.create_function(|_, args: LuaMultiValue| {
                let log_message = args
                    .into_iter()
                    .map(|v| v.to_string().unwrap_or_else(|_| "unknown".to_string()))
                    .collect::<Vec<_>>()
                    .join(" ");
                info!(target: "bevy_luau::script", "{log_message}");
                Ok(())
            })
            .unwrap(),
        )
        .unwrap();

    let ecs = lua.create_userdata(EcsHandle).unwrap();
    globals.set("Ecs", ecs).unwrap();
}

/// # Panics
pub fn load_scripts(world: &mut World) {
    let mut runtime = world
        .remove_non_send::<ScriptingRuntime>()
        .expect("ScriptingRuntime missing");
    let mut pool = world
        .remove_non_send::<EngineStringPool>()
        .expect("EngineStringPool missing");

    let mut ctx = LoadContext {
        pool: std::ptr::addr_of_mut!(pool),
        pending_components: Vec::new(),
        pending_systems: Vec::new(),
        pending_observers: Vec::new(),
        component_markers: Vec::new(),
    };

    runtime
        .lua
        .set_app_data(ScriptLoadCtx(std::ptr::addr_of_mut!(ctx)));

    set_globals(&runtime.lua);

    match std::fs::read_to_string("assets/scripts/main.luau") {
        Ok(source) => {
            if let Err(e) = runtime
                .lua
                .load(&source)
                .set_name("assets/scripts/main.luau")
                .exec()
            {
                error!("Script error: {e}");
            }
        }
        Err(e) => error!("Failed to read main.luau: {e}"),
    }

    runtime.lua.remove_app_data::<ScriptLoadCtx>();

    let mut real_ids: Vec<ComponentId> = Vec::with_capacity(ctx.pending_components.len());
    for blueprint in &ctx.pending_components {
        let (schema, descriptor) = SchemaRegistry::build(blueprint.name.clone(), &blueprint.fields);
        let id = world.register_component_with_descriptor(descriptor);
        {
            let mut reg = world.resource_mut::<SchemaRegistry>();
            if blueprint.is_resource {
                reg.resource_ids.insert(id);
                reg.resource_data
                    .insert(id, vec![0u8; schema.layout.size()]);
            }
            reg.insert(id, schema);
        }
        real_ids.push(id);
    }

    for (i, ud) in ctx.component_markers.iter().enumerate() {
        if let Ok(mut marker) = ud.borrow_mut::<LuaComponentMarker>() {
            marker.resolved_id = Some(real_ids[i]);
        }
    }

    for staged in ctx.pending_systems {
        let params = staged
            .params
            .into_iter()
            .map(|p| resolve_param(p, &real_ids))
            .collect();
        runtime.systems.push(LuaSystemDescriptor {
            func: staged.func,
            schedule: staged.schedule,
            params,
        });
    }

    for staged in ctx.pending_observers {
        let params = staged
            .params
            .into_iter()
            .map(|p| resolve_param(p, &real_ids))
            .collect();
        runtime.observers.push(LuaObserverDescriptor {
            event_id: real_ids[staged.event_idx],
            func: staged.func,
            params,
        });
    }

    world.insert_non_send(runtime);
    world.insert_non_send(pool);
}
