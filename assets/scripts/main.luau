local add = require("./module_example")

local Position = Ecs:RegisterComponent({ x = 0, y = 0, z = 0 })
local Velocity = Ecs:RegisterComponent({ x = 0, y = 0, z = 0 })
local Health = Ecs:RegisterComponent({ current = 100, max = 100 })
local EnemyTag = Ecs:RegisterComponent({})

local OnHit = Ecs:RegisterEvent({ damage = 0 })
local OnDeath = Ecs:RegisterEvent({})

local Score = Ecs:RegisterResource({ value = 0 })

local HealthQuery = Ecs:Query({
	Mutable = { Health },
})

print("ran")

function OnHitObserver(entity, hit, Commands, HealthQuery)
	local hp = HealthQuery:get(entity)
	if hp then
		hp.current -= hit.damage
		if hp.current <= 0 then
			Commands:Trigger(entity, OnDeath, {})
		end
	end
	print("hit")
end

function OnDeathObserver(entity, _, Commands)
	Commands:Despawn(entity)
	print("died")
end

Ecs:Observe(OnHit, OnHitObserver, { Ecs:Commands(), HealthQuery })
Ecs:Observe(OnDeath, OnDeathObserver, { Ecs:Commands() })

function Setup(Commands, Score)
	Commands:Spawn({
		[EnemyTag] = Ecs:Default(),
		[Health] = Ecs:Default(),
		[Position] = Ecs:Default(),
		[Velocity] = { x = 10, y = 0, z = 0 },
	})

	Score.value += 3

	print("imported from example module:", add(4, 6))

	print("setup")
end

local MoveQuery = Ecs:Query({
	Mutable = { Position },
	Immutable = { Velocity },
	With = { EnemyTag },
})

function Update(Time, Commands, MoveQuery) -- values snapshotted from rust
	local dt = Time:dt()

	for entity, pos, vel in MoveQuery do
		pos.x += vel.x * dt -- this is in pure luau and once the function finishes it flushes back to rust
		pos.y += vel.y * dt
		pos.z += vel.z * dt

		Commands:Trigger(entity, OnHit, { damage = 50 }) -- puts in a queue and flushes when function ends
		Commands:Trigger(entity, OnHit, { damage = 60 })

		print("update")
	end
end

Ecs:RegisterSystem(Setup, Ecs:Startup(), { Ecs:Commands(), Score })
Ecs:RegisterSystem(Update, Ecs:Update(), { Ecs:Time(), Ecs:Commands(), MoveQuery })
