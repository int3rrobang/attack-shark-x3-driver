export const TOP_LEVEL_HELP = `attack-shark-x11 — CLI for Attack Shark X11 gaming mouse

USAGE:
  attack-shark-x11 [global options] <command> [command options]

GLOBAL OPTIONS:
  --mode <mode>       Connection mode: x3-wired (default), x3, wired, adapter
  --delay-ms <ms>     Delay between packets in milliseconds (default: 500)
  --help, -h          Show this help

COMMANDS:
  list                List connected devices (no device open)
  open                Open and close the device to verify connectivity
  battery             Get battery level (wired modes print -1 / unavailable)
  reset               Reset device to factory defaults
  set-dpi             Configure DPI stages and active stage
  set-rate            Set polling rate (125, 250, 500, 1000 Hz)
  set-prefs           Set user preferences (lighting, RGB, sleep timers, key response)
  bind                Bind button actions
  hex <subcommand>    Build a packet and print hex without opening the device

Run 'attack-shark-x11 <command> --help' for command-specific help.

COMMON EXAMPLES:
  attack-shark-x11 list
  attack-shark-x11 open
  attack-shark-x11 set-rate 500
  attack-shark-x11 set-dpi --stages 400,800,1600 --active 2
  attack-shark-x11 set-dpi --stages 400,800,1600,2400,3200,5000,20400,26000 --active 8
  attack-shark-x11 hex dpi --stages 400,800,1600,2400,3200,26000
  attack-shark-x11 hex reset --mode adapter
`;

export const LIST_HELP = `Usage: attack-shark-x11 [global options] list

List Attack Shark X11 devices (VID 0x1d57, PIDs fa60/fa55/fa61) without opening them.
Prints mode, product name, interface, and path for each matching device.

Examples:
  attack-shark-x11 list
  bun run cli list
`;

export const OPEN_HELP = `Usage: attack-shark-x11 [global options] open

Opens the device for the selected mode, prints success, then closes it.
Useful to verify the device is reachable.

Default mode is x3-wired. Override with --mode wired or --mode adapter.

Examples:
  attack-shark-x11 open
  attack-shark-x11 --mode adapter open
  bun run cli open
`;

export const BATTERY_HELP = `Usage: attack-shark-x11 [global options] battery

Queries the battery level.  Wired modes always return -1 (unavailable).
Wireless (adapter) mode returns the battery percentage.

Examples:
  attack-shark-x11 battery
  attack-shark-x11 --mode adapter battery
`;

export const RESET_HELP = `Usage: attack-shark-x11 [global options] reset

Resets the device to factory defaults (DPI, polling rate, macros, preferences).

Examples:
  attack-shark-x11 reset
  attack-shark-x11 --mode x3-wired reset
`;

export const SET_DPI_HELP = `Usage: attack-shark-x11 [global options] set-dpi --stages a,b,c[,d,e,f,g,h] [--active 1-8]

Options:
  --stages values        X3 wired accepts 1-8 comma-separated values; X11 wired/adapter require exactly 6
  --active stage         Active DPI stage. X3 must be within provided stages; X11 is 1-6 (default: 2)
  --lod 1|2              X3 wired lift-off distance in mm
  --ripple on|off        Ripple control sensor toggle
  --angle-snap on|off    Angle snapping sensor toggle
  --motion-sync on|off   X3 wired motion sync sensor toggle

Notes:
  Default mode is x3-wired, where DPI up to 26000 is allowed.
  X11 wired/adapter modes reject DPI above 22000 and require exactly 6 stages.
  --lod and --motion-sync are X3 wired only.

Examples:
  attack-shark-x11 set-dpi --stages 400,800,1600 --active 3
  attack-shark-x11 set-dpi --stages 800,1600,2400,3200,5000,26000 --lod 2 --motion-sync on
  attack-shark-x11 set-dpi --stages 400,800,1600,2400,3200,5000,20400,26000 --active 8
  attack-shark-x11 set-dpi --stages 400,800,1600,2400,3200,26000 --active 2
  attack-shark-x11 set-dpi --stages 400,450,500,1600,1700,1800
  attack-shark-x11 --mode wired set-dpi --stages 800,1600,2400,3200,5000,22000
  bun run cli set-dpi --stages 400,800,1600,2400,3200,26000 --active 2

Preview without writing hardware:
  attack-shark-x11 hex dpi --stages 400,800,1600,2400,3200,26000 --active 2 --ripple off
`;

export const SET_RATE_HELP = `Usage: attack-shark-x11 [global options] set-rate <Hz>
       attack-shark-x11 [global options] set-rate --rate <Hz>

Options:
  --rate 125|250|500|1000   Polling rate in Hz

Examples:
  attack-shark-x11 set-rate 500
  attack-shark-x11 set-rate --rate 1000
  attack-shark-x11 --mode adapter set-rate 250
  bun run cli set-rate 500

Preview without writing hardware:
  attack-shark-x11 hex rate --rate 500
`;

export const SET_PREFS_HELP = `Usage: attack-shark-x11 [global options] set-prefs [options]

Options (all optional — unspecified fields use driver defaults; current device state is not read):
  --light off|static|breathing|neon|color-breathing|static-dpi|breathing-dpi
  --speed 1-5              LED animation speed
  --rgb r,g,b              RGB color (0-255 each channel)
  --sleep 0.5-30           Sleep time in minutes (step 0.5)
  --deep-sleep 1-60        Deep sleep time in minutes
  --key-response 4-50      Key debounce response in ms (must be even)

Examples:
  attack-shark-x11 set-prefs --light off
  attack-shark-x11 set-prefs --light neon --speed 5
  attack-shark-x11 set-prefs --light static --rgb 0,255,0
  attack-shark-x11 set-prefs --sleep 5 --deep-sleep 30 --key-response 4

Preview without writing hardware:
  attack-shark-x11 hex prefs --light neon --speed 5 --rgb 0,255,0
`;

export const BIND_HELP = `Usage: attack-shark-x11 [global options] bind --button <name> --action <macro>
       attack-shark-x11 [global options] bind --list-actions
       attack-shark-x11 [global options] bind --list-buttons

Options:
  --button left|right|middle|forward|backward|dpi|scroll-up|scroll-down
  --action <MacroName>     Action to assign (use --list-actions to see all)
  --list-actions           Print all available macro names
  --list-buttons           Print all available button names

	Note: bind sends a full model-default mapping packet; unspecified buttons are reset to model defaults, not preserved from current device state.
	On x3-wired/FA61, DPI binds appear ignored by stock firmware. Scroll binds are experimental/unsafe and may repeat actions indefinitely until unplug/reboot.

Examples:
  attack-shark-x11 bind --list-buttons
  attack-shark-x11 bind --list-actions
  attack-shark-x11 bind --button forward --action shortcut-swap-window
  attack-shark-x11 bind --button dpi --action global-disable-button
  attack-shark-x11 bind --button backward --action global-dpi-cycle

Preview without writing hardware:
  attack-shark-x11 hex bind --button forward --action shortcut-swap-window
`;

export const HEX_HELP = `Usage: attack-shark-x11 [global options] hex <subcommand> [options]

Subcommands:
  hex dpi       Build DPI packet hex (--stages, --active, --mode)
  hex rate      Build polling rate packet hex (--rate, --mode)
  hex prefs     Build preferences packet hex (--light, --speed, --rgb, ...)
  hex bind      Build button binding packet hex (--button, --action, --mode)
  hex reset     Build internal state reset packet hex (--mode)

Run 'attack-shark-x11 hex <subcommand> --help' for subcommand-specific options.

Examples:
  attack-shark-x11 hex dpi --stages 400,800,1600,2400,3200,26000
  attack-shark-x11 hex rate --rate 500
  attack-shark-x11 hex prefs --light off
  attack-shark-x11 hex bind --button forward --action shortcut-swap-window
  attack-shark-x11 hex reset --mode adapter
`;

export const HEX_DPI_HELP = `Usage: attack-shark-x11 hex dpi --stages a,b,c[,d,e,f,g,h] [--active 1-8] [--mode <mode>]

Options:
  --stages values        X3 wired accepts 1-8 values; X11 wired/adapter require exactly 6
  --active stage         Active DPI stage. X3 must be within provided stages; X11 is 1-6 (default: 2)
  --lod 1|2              X3 wired lift-off distance in mm
  --ripple on|off        Ripple control sensor toggle
  --angle-snap on|off    Angle snapping sensor toggle
  --motion-sync on|off   X3 wired motion sync sensor toggle
  --mode <mode>          Connection mode for hex output (default: global --mode or x3-wired)

Example:
  attack-shark-x11 hex dpi --stages 400,800,1600 --active 3 --mode x3-wired
  attack-shark-x11 hex dpi --stages 800,1600,2400,3200,5000,26000 --lod 2 --motion-sync on
  attack-shark-x11 hex dpi --stages 400,800,1600,2400,3200,5000,20400,26000 --active 8 --mode x3-wired
  attack-shark-x11 hex dpi --stages 800,1600,2400,3200,5000,26000 --active 2 --mode x3-wired
  attack-shark-x11 hex dpi --mode wired --stages 800,1600,2400,3200,5000,22000
`;

export const HEX_RATE_HELP = `Usage: attack-shark-x11 hex rate --rate <Hz> [--mode <mode>]

Options:
  --rate 125|250|500|1000   Polling rate
  --mode <mode>             Connection mode for hex output

Examples:
  attack-shark-x11 hex rate --rate 500
  attack-shark-x11 hex rate --mode adapter --rate 1000
`;

export const HEX_RESET_HELP = `Usage: attack-shark-x11 hex reset [--mode <mode>]

Options:
  --mode <mode>          Connection mode for hex output

Notes:
  x3-wired and wired output the observed 6-byte packet.
  adapter outputs the observed 10-byte padded packet.

Examples:
  attack-shark-x11 hex reset
  attack-shark-x11 hex reset --mode wired
  attack-shark-x11 hex reset --mode adapter
`;

export const HEX_PREFS_HELP = `Usage: attack-shark-x11 hex prefs [options] [--mode <mode>]

Options (all optional — unspecified fields use driver defaults; current device state is not read):
  --light off|static|breathing|neon|color-breathing|static-dpi|breathing-dpi
  --speed 1-5              LED animation speed
  --rgb r,g,b              RGB color
  --sleep 0.5-30           Sleep time in minutes
  --deep-sleep 1-60        Deep sleep time in minutes
  --key-response 4-50      Key debounce response (must be even)
  --mode <mode>            Connection mode for hex output

Examples:
  attack-shark-x11 hex prefs --light off
  attack-shark-x11 hex prefs --light static --rgb 255,0,128
  attack-shark-x11 hex prefs --sleep 5 --deep-sleep 30 --key-response 4
`;

export const HEX_BIND_HELP = `Usage: attack-shark-x11 hex bind --button <name> --action <macro> [--mode <mode>]

Options:
  --button left|right|middle|forward|backward|dpi|scroll-up|scroll-down
  --action <MacroName>     Action to assign
  --mode <mode>            Connection mode for hex output

Note: bind packets are full model-default mappings; unspecified buttons reset to model defaults.
On x3-wired/FA61, DPI binds appear ignored and scroll binds are experimental/unsafe.

Examples:
  attack-shark-x11 hex bind --button forward --action shortcut-swap-window
  attack-shark-x11 hex bind --button dpi --action global-disable-button
`;
