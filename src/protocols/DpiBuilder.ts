import type { BaseProtocolBuilder } from '../core/BaseProtocolBuilder.js';
import { ParamsError } from '../errors.js';
import { TransportKind } from '../types.js';

const OFFSET = Object.freeze({
	LOD: 3,
	RIPPLER_CONTROL: 4,
	STAGE_ENABLE_MASK: 5,
	ANGLE_SNAP: 6,
	MOTION_SYNC: 7,
	STAGES_START: 8,
	STAGES_HIGH_START: 16,
	CURRENT_STAGE: 24,
	FIXED_START: 25,
	CHECKSUM: 50,
});

const X3_FIXED_BYTES: readonly number[] = Object.freeze([
	0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0xff, 0xff,
	0x40, 0x00, 0xff, 0xff, 0xff, 0x01,
]);

export type StageIndex = 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8;
export type DpiStages = readonly number[];

export interface DpiBuilderOptions {
	angleSnap?: boolean;
	ripplerControl?: boolean;
	lod?: 1 | 2;
	motionSync?: boolean;
	dpiValues?: DpiStages;
	activeStage?: StageIndex;
}

/** Builder for configuring DPI and sensor parameters on X3/M600 devices. */
export class DpiBuilder implements BaseProtocolBuilder {
	public static readonly DEFAULT_OPTIONS: DpiBuilderOptions = {
		angleSnap: false,
		ripplerControl: false,
		lod: 1,
		motionSync: false,
		dpiValues: [800, 1600, 2400, 3200, 5000, 26000],
		activeStage: 2,
	};
	public static readonly X3_DEFAULT_OPTIONS: DpiBuilderOptions = DpiBuilder.DEFAULT_OPTIONS;

	readonly buffer: Buffer;
	public readonly bmRequestType = 0x21;
	public readonly bRequest = 0x09;
	public readonly wValue = 0x0304;
	public readonly wIndex = 2;
	private stages: number[] = [...(DpiBuilder.DEFAULT_OPTIONS.dpiValues ?? [])];
	private angleSnap = DpiBuilder.DEFAULT_OPTIONS.angleSnap!;
	private ripplerControl = DpiBuilder.DEFAULT_OPTIONS.ripplerControl!;
	private lod: 1 | 2 = DpiBuilder.DEFAULT_OPTIONS.lod!;
	private motionSync = DpiBuilder.DEFAULT_OPTIONS.motionSync!;

	constructor(options?: DpiBuilderOptions) {
		this.buffer = Buffer.alloc(56);
		this.buffer[0] = 0x04;
		this.buffer[1] = 0x38;
		this.buffer[2] = 0x01;
		for (let i = 0; i < X3_FIXED_BYTES.length; i++) this.buffer[OFFSET.FIXED_START + i] = X3_FIXED_BYTES[i]!;

		const config = { ...DpiBuilder.DEFAULT_OPTIONS, ...options };
		if (config.angleSnap !== undefined) this.setAngleSnap(config.angleSnap);
		if (config.ripplerControl !== undefined) this.setRipplerControl(config.ripplerControl);
		if (config.lod !== undefined) this.setLod(config.lod);
		if (config.motionSync !== undefined) this.setMotionSync(config.motionSync);
		if (config.dpiValues !== undefined) this.setStages(config.dpiValues);
		const defaultActiveStage = Math.min(
			DpiBuilder.DEFAULT_OPTIONS.activeStage ?? 2,
			this.stages.length,
		) as StageIndex;
		this.setCurrentStage(options?.activeStage ?? defaultActiveStage);
	}

	setAngleSnap(active = false): this {
		this.angleSnap = active;
		this.buffer[OFFSET.ANGLE_SNAP] = active ? 0x01 : 0x00;
		return this;
	}

	setRipplerControl(active = false): this {
		this.ripplerControl = active;
		this.buffer[OFFSET.RIPPLER_CONTROL] = active ? 0x01 : 0x00;
		return this;
	}

	setLod(lod: 1 | 2 = 1): this {
		if (lod !== 1 && lod !== 2) throw new ParamsError('lod', `LOD must be 1 or 2, got ${lod}`);
		this.lod = lod;
		this.buffer[OFFSET.LOD] = lod === 2 ? 0x01 : 0x00;
		return this;
	}

	setMotionSync(active = false): this {
		this.motionSync = active;
		this.buffer[OFFSET.MOTION_SYNC] = active ? 0x01 : 0x00;
		return this;
	}

	setCurrentStage(stage: StageIndex): this {
		this.buffer[OFFSET.CURRENT_STAGE] = stage;
		return this;
	}

	setDpiValue(stage: StageIndex, dpi: number): this {
		const index = stage - 1;
		const encoded = this.encodeDpi(dpi);
		if (this.stages.length < stage) this.stages.length = stage;
		this.stages[index] = dpi;
		this.buffer[OFFSET.STAGES_START + index] = encoded.low;
		this.buffer[OFFSET.STAGES_HIGH_START + index] = encoded.high;
		return this;
	}

	setStages(stages: DpiStages): this {
		if (!Array.isArray(stages) || stages.length < 1 || stages.length > 8)
			throw new ParamsError('stages', 'You need to pass 1 to 8 DPI values; e.g.: [800, 1600, 2400]');
		const encoded = stages.map((dpi) => this.encodeDpi(dpi));
		this.stages = [...stages];
		this.buffer.fill(0x00, OFFSET.STAGES_START, OFFSET.STAGES_HIGH_START + 8);
		for (let i = 0; i < encoded.length; i++) {
			this.buffer[OFFSET.STAGES_START + i] = encoded[i]!.low;
			this.buffer[OFFSET.STAGES_HIGH_START + i] = encoded[i]!.high;
		}
		return this;
	}

	calculateChecksum(): number {
		let sum = 0;
		for (let i = 3; i < OFFSET.CHECKSUM; i++) sum = (sum + (this.buffer[i] ?? 0x00)) & 0xffff;
		return sum;
	}

	build(transport: TransportKind): Buffer {
		this.buffer[OFFSET.LOD] = this.lod === 2 ? 0x01 : 0x00;
		this.buffer[OFFSET.RIPPLER_CONTROL] = this.ripplerControl ? 0x01 : 0x00;
		this.buffer[OFFSET.ANGLE_SNAP] = this.angleSnap ? 0x01 : 0x00;
		this.buffer[OFFSET.MOTION_SYNC] = this.motionSync ? 0x01 : 0x00;
		this.buffer[OFFSET.STAGE_ENABLE_MASK] = this.stages.length === 8 ? 0xff : (1 << this.stages.length) - 1;
		for (let i = 0; i < 8; i++) {
			const dpi = this.stages[i];
			if (dpi === undefined) {
				this.buffer[OFFSET.STAGES_START + i] = 0x00;
				this.buffer[OFFSET.STAGES_HIGH_START + i] = 0x00;
				continue;
			}
			const encoded = this.encodeDpi(dpi);
			this.buffer[OFFSET.STAGES_START + i] = encoded.low;
			this.buffer[OFFSET.STAGES_HIGH_START + i] = encoded.high;
		}
		const activeStage = this.buffer[OFFSET.CURRENT_STAGE] ?? 0;
		if (activeStage < 1 || activeStage > this.stages.length)
			throw new ParamsError(
				'activeStage',
				`Active stage ${activeStage} must be between 1 and ${this.stages.length}`,
			);
		this.buffer.writeUInt16BE(this.calculateChecksum(), OFFSET.CHECKSUM);
		return transport === TransportKind.Wired ? this.buffer.subarray(0, 52) : this.buffer;
	}

	toString(): string {
		return this.buffer.toString('hex');
	}

	compareWithHexString(value: string): boolean {
		return this.toString() === value;
	}

	private encodeDpi(dpi: number): { low: number; high: number } {
		if (!Number.isInteger(dpi) || dpi < 50 || dpi > 26000 || dpi % 50 !== 0)
			throw new ParamsError('dpi', `DPI must be an integer multiple of 50 between 50 and 26000, got ${dpi}`);
		const raw = dpi / 50 - 1;
		return { low: raw & 0xff, high: (raw >> 8) & 0xff };
	}
}
