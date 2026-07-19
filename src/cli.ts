#!/usr/bin/env bun

import { parseArgs } from './cli/args.js';
import { parseTransport } from './cli/types.js';

import * as listCmd from './cli/commands/list.js';
import * as openCmd from './cli/commands/open.js';
import * as batteryCmd from './cli/commands/battery.js';
import * as resetCmd from './cli/commands/reset.js';
import * as setDpiCmd from './cli/commands/set-dpi.js';
import * as setRateCmd from './cli/commands/set-rate.js';
import * as setPrefsCmd from './cli/commands/set-prefs.js';
import * as bindCmd from './cli/commands/bind.js';

import * as hexDpi from './cli/hex/dpi.js';
import * as hexRate from './cli/hex/rate.js';
import * as hexPrefs from './cli/hex/prefs.js';
import * as hexBind from './cli/hex/bind.js';
import * as hexReset from './cli/hex/reset.js';

import * as help from './cli/help.js';

async function main(): Promise<void> {
	const parsed = parseArgs(process.argv.slice(2));
	const transport = parseTransport(parsed.flags['transport'] as string | undefined);
	const delayMs = parsed.flags['delay-ms'] !== undefined ? Number(parsed.flags['delay-ms']) : 500;

	// top-level --help with no command
	if (parsed.flags['help'] && !parsed.command) {
		console.log(help.TOP_LEVEL_HELP);
		process.exit(0);
	}

	if (!parsed.command) {
		console.log(help.TOP_LEVEL_HELP);
		process.exit(0);
	}

	switch (parsed.command) {
		case 'list': {
			if (parsed.flags['help']) {
				console.log(listCmd.help);
				return;
			}
			await listCmd.run();
			break;
		}
		case 'open': {
			if (parsed.flags['help']) {
				console.log(openCmd.help);
				return;
			}
			await openCmd.run(transport, delayMs);
			break;
		}
		case 'battery': {
			if (parsed.flags['help']) {
				console.log(batteryCmd.help);
				return;
			}
			await batteryCmd.run(transport, delayMs);
			break;
		}
		case 'reset': {
			if (parsed.flags['help']) {
				console.log(resetCmd.help);
				return;
			}
			await resetCmd.run(transport, delayMs);
			break;
		}
		case 'set-dpi': {
			if (parsed.flags['help']) {
				console.log(setDpiCmd.help);
				return;
			}
			await setDpiCmd.run(transport, delayMs, parsed.flags);
			break;
		}
		case 'set-rate': {
			if (parsed.flags['help']) {
				console.log(setRateCmd.help);
				return;
			}
			await setRateCmd.run(transport, delayMs, parsed.flags, parsed.positionals);
			break;
		}
		case 'set-prefs': {
			if (parsed.flags['help']) {
				console.log(setPrefsCmd.help);
				return;
			}
			await setPrefsCmd.run(transport, delayMs, parsed.flags);
			break;
		}
		case 'bind': {
			if (parsed.flags['help']) {
				console.log(bindCmd.help);
				return;
			}
			await bindCmd.run(transport, delayMs, parsed.flags);
			break;
		}
		case 'hex': {
			const sub = parsed.positionals[0];
			if (!sub) {
				console.log(help.HEX_HELP);
				process.exit(0);
			}

			// re-parse --transport within hex flags if present, otherwise inherit
			const hexTransport = parseTransport(parsed.flags['transport'] as string | undefined);

			if (parsed.flags['help']) {
				switch (sub) {
					case 'dpi':
						console.log(hexDpi.help);
						return;
					case 'rate':
						console.log(hexRate.help);
						return;
					case 'prefs':
						console.log(hexPrefs.help);
						return;
					case 'bind':
						console.log(hexBind.help);
						return;
					case 'reset':
						console.log(hexReset.help);
						return;
					default:
						console.log(help.HEX_HELP);
						return;
				}
			}

			switch (sub) {
				case 'dpi':
					hexDpi.run(hexTransport, parsed.flags);
					break;
				case 'rate':
					hexRate.run(hexTransport, parsed.flags);
					break;
				case 'prefs':
					hexPrefs.run(hexTransport, parsed.flags);
					break;
				case 'bind':
					hexBind.run(hexTransport, parsed.flags);
					break;
				case 'reset':
					hexReset.run(hexTransport);
					break;
				default:
					console.error(`Unknown hex subcommand: ${sub}`);
					console.log(help.HEX_HELP);
					process.exit(1);
			}
			break;
		}
		default: {
			console.error(`Unknown command: ${parsed.command}`);
			console.log(help.TOP_LEVEL_HELP);
			process.exit(1);
		}
	}
}

main().catch((err: unknown) => {
	const message = err instanceof Error ? err.message : String(err);
	console.error('Error:', message);
	if (err instanceof Error && err.cause instanceof Error) {
		console.error('Cause:', err.cause.message);
	} else if (err instanceof Error && err.cause !== undefined && err.cause !== null) {
		console.error('Cause:', String(err.cause));
	}
	process.exit(1);
});
