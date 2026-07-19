export const TOP_LEVEL_HELP = `attack-shark-x3 — CLI for Attack Shark X3/M600 gaming mouse

USAGE:
  attack-shark-x3 [global options] <command> [command options]

GLOBAL OPTIONS:
  --transport <transport>  USB transport: wired (default) or receiver
  --delay-ms <ms>          Delay between packets in milliseconds (default: 500)
  --help, -h               Show this help

COMMANDS:
  list                List connected devices (no device open)
  open                Open and close the device to verify connectivity
  battery             Get battery level (wired transport prints -1 / unavailable)
  reset               Reset device to factory defaults
  set-dpi             Configure DPI stages and active stage
  set-rate            Set polling rate (125, 250, 500, 1000 Hz)
  set-prefs           Set user preferences (lighting, RGB, sleep timers, key response)
  bind                Bind button actions
  hex <subcommand>    Build a packet and print hex without opening the device

Run 'attack-shark-x3 <command> --help' for command-specific help.

COMMON EXAMPLES:
  attack-shark-x3 list
  attack-shark-x3 open
  attack-shark-x3 --transport receiver battery
  attack-shark-x3 set-rate 500
  attack-shark-x3 set-dpi --stages 400,800,1600 --active 2
  attack-shark-x3 set-dpi --stages 400,800,1600,2400,3200,5000,20400,26000 --active 8
  attack-shark-x3 hex dpi --stages 400,800,1600,2400,3200,5000,20400,26000
  attack-shark-x3 hex reset --transport receiver
`;

export const LIST_HELP = `Usage: attack-shark-x3 [global options] list

List Attack Shark X3/M600 devices (VID 0x1d57, PIDs fa60/fa61) without opening them.
Prints transport, product name, interface, and path for each matching device.

Examples:
  attack-shark-x3 list
  bun run cli list
`;

export const OPEN_HELP = `Usage: attack-shark-x3 [global options] open

Opens the device for the selected transport, prints success, then closes it.
Useful to verify the device is reachable.

Default transport is wired. Override with --transport receiver.

Examples:
  attack-shark-x3 open
  attack-shark-x3 --transport receiver open
  bun run cli open
`;

export const BATTERY_HELP = `Usage: attack-shark-x3 [global options] battery

Queries the battery level. Wired transport returns -1 (unavailable).
Receiver transport returns the battery percentage.

Examples:
  attack-shark-x3 battery
  attack-shark-x3 --transport receiver battery
`;

export const RESET_HELP = `Usage: attack-shark-x3 [global options] reset

Resets the device to factory defaults (DPI, polling rate, macros, preferences).

Examples:
  attack-shark-x3 reset
  attack-shark-x3 --transport receiver reset
`;

export const SET_DPI_HELP = `Usage: attack-shark-x3 [global options] set-dpi --stages a,b,c[,d,e,f,g,h] [--active 1-8]

Options:
  --stages values        One to eight comma-separated DPI values
  --active stage         Active DPI stage, 1-8 (default: 2)
  --lod 1|2              Lift-off distance in mm
  --ripple on|off        Ripple control sensor toggle
  --angle-snap on|off    Angle snapping sensor toggle
  --motion-sync on|off   Motion sync sensor toggle

Notes:
  DPI up to 26000 is allowed. Both wired and receiver transports use X3 sensor options.

Examples:
  attack-shark-x3 set-dpi --stages 400,800,1600 --active 3
  attack-shark-x3 set-dpi --stages 800,1600,2400,3200,5000,26000 --lod 2 --motion-sync on
  attack-shark-x3 set-dpi --stages 400,800,1600,2400,3200,5000,20400,26000 --active 8
  attack-shark-x3 --transport receiver set-dpi --stages 400,800,1600,2400,3200,26000

Preview without writing hardware:
  attack-shark-x3 hex dpi --stages 400,800,1600,2400,3200,5000,20400,26000 --active 8 --ripple off
`;

export const SET_RATE_HELP = `Usage: attack-shark-x3 [global options] set-rate <Hz>
       attack-shark-x3 [global options] set-rate --rate <Hz>

Options:
  --rate 125|250|500|1000   Polling rate in Hz

Examples:
  attack-shark-x3 set-rate 500
  attack-shark-x3 set-rate --rate 1000
  attack-shark-x3 --transport receiver set-rate 250
  bun run cli set-rate 500

Preview without writing hardware:
  attack-shark-x3 hex rate --rate 500
`;

export const SET_PREFS_HELP = `Usage: attack-shark-x3 [global options] set-prefs [options]

Options (all optional — unspecified fields use driver defaults; current device state is not read):
  --light off|static|breathing|neon|color-breathing|static-dpi|breathing-dpi
  --speed 1-5              LED animation speed
  --rgb r,g,b              RGB color (0-255 each channel)
  --sleep 0.5-30           Sleep time in minutes (step 0.5)
  --deep-sleep 1-60        Deep sleep time in minutes
  --key-response 4-50      Key debounce response in ms (must be even)

Examples:
  attack-shark-x3 set-prefs --light off
  attack-shark-x3 set-prefs --light neon --speed 5
  attack-shark-x3 set-prefs --light static --rgb 0,255,0
  attack-shark-x3 set-prefs --sleep 5 --deep-sleep 30 --key-response 4

Preview without writing hardware:
  attack-shark-x3 hex prefs --light neon --speed 5 --rgb 0,255,0
`;

export const BIND_HELP = `Usage: attack-shark-x3 [global options] bind --button <name> --action <macro>
       attack-shark-x3 [global options] bind --list-actions
       attack-shark-x3 [global options] bind --list-buttons

Options:
  --button left|right|middle|forward|backward|dpi|scroll-up|scroll-down
  --action <MacroName>     Action to assign (use --list-actions to see all)
  --list-actions           Print all available macro names
  --list-buttons           Print all available button names

Note: bind sends a full model-default mapping packet; unspecified buttons are reset to model defaults.
On FA61 wired, DPI binds appear ignored by stock firmware. Scroll binds are experimental/unsafe.

Examples:
  attack-shark-x3 bind --list-buttons
  attack-shark-x3 bind --list-actions
  attack-shark-x3 bind --button forward --action shortcut-swap-window
  attack-shark-x3 bind --button dpi --action global-disable-button
  attack-shark-x3 bind --button backward --action global-dpi-cycle

Preview without writing hardware:
  attack-shark-x3 hex bind --button forward --action shortcut-swap-window
`;

export const HEX_HELP = `Usage: attack-shark-x3 [global options] hex <subcommand> [options]

Subcommands:
  hex dpi       Build DPI packet hex (--stages, --active, --transport)
  hex rate      Build polling rate packet hex (--rate, --transport)
  hex prefs     Build preferences packet hex (--light, --speed, --rgb, ...)
  hex bind      Build button binding packet hex (--button, --action, --transport)
  hex reset     Build internal state reset packet hex (--transport)

Run 'attack-shark-x3 hex <subcommand> --help' for subcommand-specific options.

Examples:
  attack-shark-x3 hex dpi --stages 400,800,1600,2400,3200,5000,20400,26000
  attack-shark-x3 hex rate --rate 500
  attack-shark-x3 hex prefs --light off
  attack-shark-x3 hex bind --button forward --action shortcut-swap-window
  attack-shark-x3 hex reset --transport receiver
`;

export const HEX_DPI_HELP = `Usage: attack-shark-x3 hex dpi --stages a,b,c[,d,e,f,g,h] [--active 1-8] [--transport <transport>]

Options:
  --stages values        One to eight DPI values
  --active stage         Active DPI stage, 1-8 (default: 2)
  --lod 1|2              Lift-off distance in mm
  --ripple on|off        Ripple control sensor toggle
  --angle-snap on|off    Angle snapping sensor toggle
  --motion-sync on|off   Motion sync sensor toggle
  --transport <transport>  Transport for hex output (default: wired)

Example:
  attack-shark-x3 hex dpi --stages 400,800,1600 --active 3 --transport receiver
  attack-shark-x3 hex dpi --stages 800,1600,2400,3200,5000,26000 --lod 2 --motion-sync on
  attack-shark-x3 hex dpi --stages 400,800,1600,2400,3200,5000,20400,26000 --active 8
`;

export const HEX_RATE_HELP = `Usage: attack-shark-x3 hex rate --rate <Hz> [--transport <transport>]

Options:
  --rate 125|250|500|1000   Polling rate
  --transport <transport>  Transport for hex output (default: wired)

Examples:
  attack-shark-x3 hex rate --rate 500
  attack-shark-x3 hex rate --transport receiver --rate 1000
`;

export const HEX_RESET_HELP = `Usage: attack-shark-x3 hex reset [--transport <transport>]

Options:
  --transport <transport>  Transport for hex output (default: wired)

Notes:
  Wired output is the compact 6-byte packet.
  Receiver output is the padded 10-byte packet.

Examples:
  attack-shark-x3 hex reset
  attack-shark-x3 hex reset --transport wired
  attack-shark-x3 hex reset --transport receiver
`;

export const HEX_PREFS_HELP = `Usage: attack-shark-x3 hex prefs [options] [--transport <transport>]

Options (all optional — unspecified fields use driver defaults; current device state is not read):
  --light off|static|breathing|neon|color-breathing|static-dpi|breathing-dpi
  --speed 1-5              LED animation speed
  --rgb r,g,b              RGB color
  --sleep 0.5-30           Sleep time in minutes
  --deep-sleep 1-60        Deep sleep time in minutes
  --key-response 4-50      Key debounce response (must be even)
  --transport <transport>  Transport for hex output (default: wired)

Examples:
  attack-shark-x3 hex prefs --light off
  attack-shark-x3 hex prefs --light static --rgb 255,0,128
  attack-shark-x3 hex prefs --sleep 5 --deep-sleep 30 --key-response 4
`;

export const HEX_BIND_HELP = `Usage: attack-shark-x3 hex bind --button <name> --action <macro> [--transport <transport>]

Options:
  --button left|right|middle|forward|backward|dpi|scroll-up|scroll-down
  --action <MacroName>     Action to assign
  --transport <transport>  Transport for hex output (default: wired)

Note: bind packets are full model-default mappings; unspecified buttons reset to model defaults.
On FA61 wired, DPI binds appear ignored and scroll binds are experimental/unsafe.

Examples:
  attack-shark-x3 hex bind --button forward --action shortcut-swap-window
  attack-shark-x3 hex bind --button dpi --action global-disable-button
`;
