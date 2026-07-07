export interface ParsedArgs {
	flags: Record<string, string | boolean>;
	command: string | null;
	positionals: string[];
}

/**
 * Minimal argv parser — no dependencies.
 * Handles --key value, --boolean-flag, -h, and positional arguments.
 */
export function parseArgs(argv: string[]): ParsedArgs {
	const flags: Record<string, string | boolean> = {};
	const positionals: string[] = [];

	let i = 0;
	while (i < argv.length) {
		const arg = argv[i]!;
		if (arg === '--help' || arg === '-h') {
			flags['help'] = true;
			i++;
		} else if (arg.startsWith('--')) {
			const key = arg.slice(2);
			const next: string | undefined = argv[i + 1];
			if (next === undefined || next.startsWith('--') || next.startsWith('-')) {
				flags[key] = true;
				i++;
			} else {
				flags[key] = next;
				i += 2;
			}
		} else if (arg.startsWith('-') && arg.length > 1) {
			// short flag like -h
			flags[arg.slice(1)] = true;
			i++;
		} else {
			positionals.push(arg);
			i++;
		}
	}

	const command = positionals.length > 0 ? (positionals[0] ?? null) : null;
	return { flags, command, positionals: positionals.slice(1) };
}
