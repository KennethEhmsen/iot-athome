# Example automation rules

The `iot-automation` engine loads YAML rules from a configurable
directory at startup. These examples show the canonical shapes the
engine recognises:

| File | Trigger family | Demonstrates |
|------|---------------|--------------|
| `kitchen-fan-hot.yaml` | `device.>` (M3) | Sensor-state → command publish, with a CEL `when` clause |
| `lights-on-by-intent.yaml` | `command.intent.>` (M5b W5) | Voice-or-UI intent → device command. Mirror of how `iot-voice send "turn on the kitchen light"` makes a real bulb light up. |
| `cancel-on-intent.yaml` | `command.intent.>` (M5b W5) | System-cancel intent → broadcast a stop signal on `cmd.system.cancel` |

## Loading

The engine's config (see `iot-automation`'s default in `iot-config`) points at
a rules directory; drop a YAML file in there and restart:

```sh
mkdir -p /var/lib/iotathome/rules
cp examples/rules/lights-on-by-intent.yaml /var/lib/iotathome/rules/
systemctl restart iot-automation
```

For dev: `cargo run -p iot-automation -- --rules-dir examples/rules` (or
whatever the binary's flag is — check `iot-automation --help`).

## Testing one before deploying

```sh
iotctl rule test --rule examples/rules/lights-on-by-intent.yaml \
  --trigger 'command.intent.lights.on' \
  --payload '{"domain":"lights","verb":"on","args":{"target":"kitchen"},"raw":"turn on the kitchen light","confidence":0.95}'
```

`iotctl rule test` exercises the same dispatch path the engine uses
without touching the bus — fast iteration on rule YAML before commit.

## Sending a test intent end-to-end

```sh
# Brings the action up via the real bus. Requires the dev stack running
# (`just dev`) and `iot-voice` configured for it.
iot-voice send 'turn on the kitchen light'
# Watch the bus to see the rule fire:
nats sub 'cmd.>' &
nats sub 'command.intent.>' &
```
