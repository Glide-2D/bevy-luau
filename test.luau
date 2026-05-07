local Ecs = 0; -- silences type errors for lsp

local Position = Ecs:RegisterComponent { x = 0, y = 0, z = 0 }
local Velocity = Ecs:RegisterComponent { x = 0, y = 0, z = 0 }
local Health = Ecs:RegisterComponent { current = 100, max = 100 }
local EnemyTag = Ecs:RegisterComponent {}

local OnHit = Ecs:RegisterEvent { damage = 0 }
local OnDeath = Ecs:RegisterEvent {}

local Score = Ecs:RegisterResource({ value = 0 })

local HealthQuery = Ecs:Query {
	Mutable = { Health },
}

Ecs:Require(EnemyTag, { Position, Velocity, Health })

function OnHitObserver(entity, hit, Commands, HealthQuery)
	local hp = HealthQuery:get(entity)
	if hp then
		hp.current -= hit.damage
		if hp.current <= 0 then
			Commands:Trigger(entity, OnDeath, {})
		end
	end
end

function OnDeathObserver(entity, _, Commands)
	Commands:Despawn(entity)
end

Ecs:Observe(OnHit, OnHitObserver, { Ecs:Commands(), HealthQuery })
Ecs:Observe(OnDeath, OnDeathObserver, { Ecs:Commands() })

function Setup(Commands, Score)
	Commands:Spawn {
		[EnemyTag] = {},
		[Velocity] = { x = 10, y = 0, z = 0 },
	}

	Score.value += 3
end

local MoveQuery = Ecs:Query {
	Mutable = { Position },
	Immutable = { Velocity },
	With = { EnemyTag },
}

function Update(Time, Commands, MoveQuery)
	local dt = Time.dt()

	for entity, pos, vel in MoveQuery do
		pos.x += vel.x * dt
		pos.y += vel.y * dt
		pos.z += vel.z * dt

		Commands:Trigger(entity, OnHit, { damage = 50 })
		Commands:Trigger(entity, OnHit, { damage = 60 })
	end
end

Ecs:RegisterSystem(Setup, Ecs:Startup(), { Ecs:Time(), Ecs:Commands(), Score })
Ecs:RegisterSystem(Update, Ecs:Update(), { Ecs:Time(), Ecs:Commands(), MoveQuery })