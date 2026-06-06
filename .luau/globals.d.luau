--!strict

declare extern type LuaComponentMarker with end
declare extern type LuaResourceMarker with end
declare extern type ScheduleMarker with end
declare extern type QueryDescHandle with end
declare extern type CommandsParam with end
declare extern type TimeParam with end
declare extern type DefaultMarker with end

type LuaParamMarker = CommandsParam | TimeParam | QueryDescHandle | LuaResourceMarker

declare extern type LuaTime with
	function dt(self): number
	function elapsed(self): number
end

declare extern type LuaCommandsHandle with
	function Spawn(self, components: { [LuaComponentMarker]: { [string]: any }? }): ()
	function Despawn(self, entity_bits: number): ()
	function Trigger(self, entity_bits: number, event_id: LuaComponentMarker, data: { [string]: any }): ()
end

declare extern type QuerySnapshot with
	function get(self, entity_bits: number): ...{ [string]: any }
end

type QueryDef = {
	Mutable: { LuaComponentMarker }?,
	Immutable: { LuaComponentMarker }?,
	With: { LuaComponentMarker }?,
	Without: { LuaComponentMarker }?,
}

declare extern type EcsHandle with
	-- Parameter Factories
	function Startup(self): ScheduleMarker
	function Update(self): ScheduleMarker
	function Commands(self): CommandsParam
	function Time(self): TimeParam
	function Default(self): DefaultMarker
	
	function RegisterComponent(self, schema_table: { [string]: any }): LuaComponentMarker
	function RegisterEvent(self, schema_table: { [string]: any }): LuaComponentMarker
	function RegisterResource(self, schema_table: { [string]: any }): LuaResourceMarker
	
	function Query(self, def: QueryDef): QueryDescHandle
	
	function RegisterSystem(
		self, 
		func: (...any) -> (), 
		sched_ud: ScheduleMarker, 
		params_tbl: { LuaParamMarker }
	): ()
	
	function Observe(
		self, 
		event_ud: LuaComponentMarker, 
		func: (...any) -> (), 
		params_tbl: { LuaParamMarker }
	): ()
end

declare Ecs: EcsHandle