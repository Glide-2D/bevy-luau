#![expect(
    unsafe_code,
    reason = "Unsafe code is needed to work with dynamic components"
)]

use bevy::{
    ecs::{
        component::{ComponentCloneBehavior, ComponentDescriptor, ComponentId, StorageType},
        query::QueryBuilder,
        world::FilteredEntityMut,
    },
    prelude::*,
    ptr::OwningPtr,
};
use lasso::{Rodeo, Spur};
use mluau::prelude::*;
use std::{
    alloc::{Layout, alloc_zeroed, dealloc},
    collections::{HashMap, HashSet},
    ptr::NonNull,
};

pub struct ScriptingPlugin;

impl Plugin for ScriptingPlugin {
    fn build(&self, app: &mut App) {
        app.init_non_send::<ScriptingRuntime>()
            .init_non_send::<EngineStringPool>()
            .init_resource::<SchemaRegistry>()
            .add_systems(Startup, (load_scripts, lua_startup_system).chain())
            .add_systems(Update, lua_update_system);
    }
}

#[derive(Clone, Copy)]
pub enum LuaSchedule {
    Startup,
    Update,
}

#[derive(Clone, Default)]
pub struct ResolvedQuery {
    pub mutable: Vec<ComponentId>,
    pub immutable: Vec<ComponentId>,
    pub with: Vec<ComponentId>,
    pub without: Vec<ComponentId>,
}

#[derive(Clone)]
pub enum LuaParam {
    Commands,
    Time,
    Query(ResolvedQuery),
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

pub struct ScriptingRuntime {
    pub lua: Lua,
    pub systems: Vec<LuaSystemDescriptor>,
    pub observers: Vec<LuaObserverDescriptor>,
}

impl Default for ScriptingRuntime {
    fn default() -> Self {
        ScriptingRuntime {
            lua: Lua::new(),
            systems: Vec::new(),
            observers: Vec::new(),
        }
    }
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

    pub fn intern(&mut self, lua: &Lua, s: &str) -> Spur {
        let spur = self.rodeo.get_or_intern(s);
        self.bridge
            .entry(spur)
            .or_insert_with(|| lua.create_string(s).unwrap());
        spur
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

fn align_up(offset: usize, align: usize) -> usize {
    (offset + align - 1) & !(align - 1)
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
    pub resource_ids: HashSet<ComponentId>,
    pub resource_data: HashMap<ComponentId, Vec<u8>>,
}

impl SchemaRegistry {
    pub fn build(
        name: String,
        fields: &[(Spur, LuauFieldType)],
    ) -> (DynamicComponentSchema, ComponentDescriptor) {
        let mut offset = 0usize;
        let mut field_offsets = HashMap::new();

        for &(spur, ft) in fields {
            let layout = ft.layout();
            offset = align_up(offset, layout.align());
            field_offsets.insert(spur, (offset, ft));
            offset += layout.size();
        }

        let align = fields
            .iter()
            .map(|(_, t)| t.layout().align())
            .max()
            .unwrap_or(1);
        let size = align_up(offset, align).max(1);
        let layout = Layout::from_size_align(size, align).expect("invalid layout");

        let schema = DynamicComponentSchema {
            name: name.clone(),
            fields: field_offsets,
            layout,
        };

        let descriptor = unsafe {
            ComponentDescriptor::new_with_layout(
                name,
                StorageType::Table,
                layout,
                None,
                true,
                ComponentCloneBehavior::Ignore,
                None,
            )
        };

        (schema, descriptor)
    }

    pub fn insert(&mut self, id: ComponentId, schema: DynamicComponentSchema) {
        self.name_to_id.insert(schema.name.clone(), id);
        self.id_to_schema.insert(id, schema);
    }
}

struct ComponentBlueprint {
    name: String,
    fields: Vec<(Spur, LuauFieldType)>,
    is_resource: bool,
}

#[derive(Clone, Default)]
struct StagedQuery {
    mutable: Vec<usize>,
    immutable: Vec<usize>,
    with: Vec<usize>,
    without: Vec<usize>,
}

#[derive(Clone)]
enum StagedParam {
    Commands,
    Time,
    Query(StagedQuery),
    Resource(usize),
}

struct StagedSystem {
    func: LuaFunction,
    schedule: LuaSchedule,
    params: Vec<StagedParam>,
}

struct StagedObserver {
    event_idx: usize,
    func: LuaFunction,
    params: Vec<StagedParam>,
}

struct LoadContext {
    pool: *mut EngineStringPool,
    pending_components: Vec<ComponentBlueprint>,
    pending_systems: Vec<StagedSystem>,
    pending_observers: Vec<StagedObserver>,
    component_markers: Vec<LuaAnyUserData>,
}

struct ScriptLoadCtx(*mut LoadContext);

fn with_ctx<T>(lua: &Lua, f: impl FnOnce(&mut LoadContext) -> LuaResult<T>) -> LuaResult<T> {
    let ptr = {
        let guard = lua
            .app_data_ref::<ScriptLoadCtx>()
            .ok_or_else(|| LuaError::runtime("Ecs API only available during script loading"))?;
        guard.0
    };
    f(unsafe { &mut *ptr })
}

struct LuaComponentMarker {
    staging_idx: usize,
    resolved_id: Option<ComponentId>,
}

impl LuaComponentMarker {
    fn component_id(&self) -> LuaResult<ComponentId> {
        self.resolved_id
            .ok_or_else(|| LuaError::runtime("component marker not yet resolved"))
    }
}

impl LuaUserData for LuaComponentMarker {}

#[derive(Clone, Copy)]
struct ScheduleMarker(LuaSchedule);
impl LuaUserData for ScheduleMarker {}

struct CommandsParam;
struct TimeParam;
struct DefaultMarker;

#[derive(Clone, Copy)]
struct ResourceDesc(usize);

impl LuaUserData for CommandsParam {}
impl LuaUserData for TimeParam {}
impl LuaUserData for DefaultMarker {}
impl LuaUserData for ResourceDesc {}

struct QueryDescHandle(StagedQuery);
impl LuaUserData for QueryDescHandle {}

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

#[derive(Default)]
struct CommandBuffer {
    spawns: Vec<SpawnCmd>,
    despawns: Vec<Entity>,
    triggers: Vec<TriggerCmd>,
}

struct SpawnCmd {
    components: Vec<(ComponentId, Option<LuaTable>)>,
}

struct TriggerCmd {
    entity: Entity,
    event_id: ComponentId,
    data_table: LuaTable,
}

struct LuaCommandsHandle(*mut CommandBuffer);

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
                    LuaValue::UserData(ud) if ud.is::<DefaultMarker>() => None,
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

#[derive(Clone)]
struct SnapshotRow {
    entity: Entity,
    mutable_tables: Vec<LuaTable>,
    immutable_tables: Vec<LuaTable>,
}

struct QuerySnapshot {
    desc: ResolvedQuery,
    rows: Vec<SnapshotRow>,
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
            let read_staging_ids = |key: &str| -> LuaResult<Vec<usize>> {
                let t: Option<LuaTable> = def.get(key)?;
                match t {
                    Some(t) => t
                        .sequence_values::<LuaAnyUserData>()
                        .map(|v| Ok(v?.borrow::<LuaComponentMarker>()?.staging_idx))
                        .collect(),
                    None => Ok(Vec::new()),
                }
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
) -> LuaResult<Vec<(Spur, LuauFieldType)>> {
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

fn parse_staged_params(table: &LuaTable) -> LuaResult<Vec<StagedParam>> {
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

fn snapshot_query(
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

fn writeback_snapshot(
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

fn extract_resource_table(
    registry: &SchemaRegistry,
    pool: &EngineStringPool,
    lua: &Lua,
    id: ComponentId,
) -> LuaResult<Option<LuaTable>> {
    let Some(data) = registry.resource_data.get(&id) else {
        return Ok(None);
    };
    let Some(schema) = registry.id_to_schema.get(&id) else {
        return Ok(None);
    };

    let table = lua.create_table()?;
    for (&spur, &(offset, ft)) in &schema.fields {
        let lua_str = pool.get_lua_str(spur);
        let field_ptr = unsafe { data.as_ptr().add(offset) };
        match ft {
            LuauFieldType::Bool => table.raw_set(lua_str, unsafe { *field_ptr.cast::<bool>() })?,
            LuauFieldType::Integer => {
                table.raw_set(lua_str, unsafe { *field_ptr.cast::<i64>() })?
            }
            LuauFieldType::Number => table.raw_set(lua_str, unsafe { *field_ptr.cast::<f64>() })?,
            LuauFieldType::Vector4 => {
                let v = unsafe { *field_ptr.cast::<[f32; 4]>() };
                table.raw_set(lua_str, LuaVector::new(v[0], v[1], v[2], v[3]))?;
            }
            LuauFieldType::String => {
                let spur = unsafe { *field_ptr.cast::<Spur>() };
                table.raw_set(lua_str, pool.get_lua_str(spur))?;
            }
            LuauFieldType::Buffer(len) => {
                let slice = unsafe { std::slice::from_raw_parts(field_ptr, len) };
                table.raw_set(lua_str, lua.create_buffer(slice)?)?;
            }
        }
    }
    Ok(Some(table))
}

fn writeback_resource_table(
    registry: &mut SchemaRegistry,
    pool: &EngineStringPool,
    id: ComponentId,
    table: &LuaTable,
) -> LuaResult<()> {
    let fields: Vec<(Spur, usize, LuauFieldType)> = match registry.id_to_schema.get(&id) {
        Some(s) => s
            .fields
            .iter()
            .map(|(&sp, &(off, ft))| (sp, off, ft))
            .collect(),
        None => return Ok(()),
    };

    let data = match registry.resource_data.get_mut(&id) {
        Some(d) => d,
        None => return Ok(()),
    };

    for (spur, offset, ft) in fields {
        let lua_str = pool.get_lua_str(spur);
        let field_ptr = unsafe { data.as_mut_ptr().add(offset) };
        match (table.raw_get::<LuaValue>(lua_str)?, ft) {
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
            _ => {}
        }
    }
    Ok(())
}

fn run_lua_system(
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
                    .create_userdata(LuaCommandsHandle(cmd_ptr))
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

fn run_lua_observer(
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
                    .create_userdata(LuaCommandsHandle(cmd_ptr))
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

fn load_scripts(world: &mut World) {
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

    let globals = runtime.lua.globals();

    let ecs = runtime.lua.create_userdata(EcsHandle).unwrap();
    globals.set("Ecs", ecs).unwrap();

    globals
        .set(
            "print",
            runtime
                .lua
                .create_function(|_, args: LuaMultiValue| {
                    let mut output = Vec::new();

                    for value in args.into_iter() {
                        let str_val = match value {
                            LuaValue::Nil => "nil".to_string(),
                            LuaValue::Boolean(b) => b.to_string(),
                            LuaValue::Integer(i) => i.to_string(),
                            LuaValue::Number(n) => n.to_string(),
                            LuaValue::String(s) => s.to_string_lossy(),
                            LuaValue::Table(_) => "table".to_string(),
                            LuaValue::Function(_) => "function".to_string(),
                            LuaValue::UserData(_) => "userdata".to_string(),
                            _ => "unknown".to_string(),
                        };
                        output.push(str_val);
                    }

                    let log_message = output.join(" ");

                    info!(target: "bevy_luau::script", "{log_message}");

                    Ok(())
                })
                .unwrap(),
        )
        .unwrap();

    match std::fs::read_to_string("assets/scripts/init.luau") {
        Ok(source) => {
            if let Err(e) = runtime.lua.load(&source).exec() {
                error!("Script error: {e}");
            }
        }
        Err(e) => error!("Failed to read init.luau: {e}"),
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

fn lua_startup_system(world: &mut World) {
    let runtime = world
        .remove_non_send::<ScriptingRuntime>()
        .expect("ScriptingRuntime missing");
    let mut pool = world
        .remove_non_send::<EngineStringPool>()
        .expect("EngineStringPool missing");

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

fn lua_update_system(world: &mut World) {
    let runtime = world
        .remove_non_send::<ScriptingRuntime>()
        .expect("ScriptingRuntime missing");
    let mut pool = world
        .remove_non_send::<EngineStringPool>()
        .expect("EngineStringPool missing");

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

pub struct DynamicComponentBridge;

impl DynamicComponentBridge {
    /// # Safety
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

        let layout = schema.layout;
        let scratch = unsafe { alloc_zeroed(layout) };
        if scratch.is_null() {
            std::alloc::handle_alloc_error(layout);
        }

        for (&spur, &(offset, ft)) in &schema.fields {
            let lua_str = pool.get_lua_str(spur);
            let field_ptr = unsafe { scratch.add(offset) };
            match (table.raw_get::<LuaValue>(lua_str)?, ft) {
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
                    let sp = pool.intern(lua, s.to_str()?.as_ref());
                    unsafe { std::ptr::write(field_ptr.cast::<Spur>(), sp) };
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

        let owning = unsafe { OwningPtr::new(NonNull::new_unchecked(scratch)) };
        unsafe { world.entity_mut(entity).insert_by_id(component_id, owning) };
        unsafe { dealloc(scratch, layout) };
        Ok(())
    }

    /// # Safety
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
        let layout = schema.layout;
        let scratch = unsafe { alloc_zeroed(layout) };
        if scratch.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        let owning = unsafe { OwningPtr::new(NonNull::new_unchecked(scratch)) };
        unsafe { world.entity_mut(entity).insert_by_id(component_id, owning) };
        unsafe { dealloc(scratch, layout) };
    }

    /// # Safety
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
                    table.raw_set(lua_str, unsafe { *field_ptr.cast::<bool>() })?
                }
                LuauFieldType::Integer => {
                    table.raw_set(lua_str, unsafe { *field_ptr.cast::<i64>() })?
                }
                LuauFieldType::Number => {
                    table.raw_set(lua_str, unsafe { *field_ptr.cast::<f64>() })?
                }
                LuauFieldType::Vector4 => {
                    let v = unsafe { *field_ptr.cast::<[f32; 4]>() };
                    table.raw_set(lua_str, LuaVector::new(v[0], v[1], v[2], v[3]))?;
                }
                LuauFieldType::String => {
                    let sp = unsafe { *field_ptr.cast::<Spur>() };
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
#[cfg(test)]
mod tests {
    use super::*;
    use std::f64;
    #[test]
    fn roundtrip() {
        let lua = Lua::new();
        let mut world = World::new();
        let mut pool = EngineStringPool::default();
        let mut registry = SchemaRegistry::default();

        let fields = vec![
            (pool.intern(&lua, "a"), LuauFieldType::Integer),
            (pool.intern(&lua, "b"), LuauFieldType::Number),
        ];

        let (schema, descriptor) = SchemaRegistry::build("Test".into(), &fields);
        let id = world.register_component_with_descriptor(descriptor);
        registry.insert(id, schema);

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
            assert!((out.get::<f64>("b").unwrap() - f64::consts::PI).abs() < 1e-9);
        }
    }
}
