import type { BaseProtocolBuilder } from '../core/BaseProtocolBuilder.js';
import { ParamsError } from '../errors.js';
import { DPI_STEP_MAP } from '../tables/dpi-map.js';
import { ConnectionMode, isConnectionModeWired } from '../types.js';

const OFFSET = Object.freeze({
	ANGLE_SNAP: 3,
	X3_LOD: 3,
	RIPPLER_CONTROL: 4,
	STAGE_ENABLE_MASK: 5,
	STAGE_MASK_A: 6,
	X3_ANGLE_SNAP: 6,
	STAGE_MASK_B: 7,
	X3_MOTION_SYNC: 7,
	X3_STAGES_HIGH_START: 16,
	EXPANDED_MASK: 16,
	CURRENT_STAGE: 24,
	CHECKSUM_HIGH_BYTE: 50,
	CHECKSUM_LOW_BYTE: 51,
	STAGES_START: 8,
});

/**
 * Represents a stage index that can have one of the preset integer values.
 *
 * This type is used to define a sequential stage in a process or workflow.
 * It restricts the possible values to integers 1 through 8.
 */
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

/**
 * Original X11 fixed bytes at offsets 25-49.
 * Applied during build for ConnectionMode.Wired / ConnectionMode.Adapter.
 */
export const X11_FIXED_BYTES: readonly number[] = Object.freeze([
	0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0xff, 0xff,
	0x40, 0x00, 0xff, 0xff, 0xff, 0x02,
]);

/**
 * X3 variant fixed bytes at offsets 25-49.
 * Applied during build for ConnectionMode.X3Wired.
 */
const X3_FIXED_BYTES: readonly number[] = Object.freeze([
	0xff, 0x00, 0x00, 0x00, 0xff, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0x00, 0xff, 0xff, 0xff, 0x00, 0xff, 0xff,
	0x40, 0x00, 0xff, 0xff, 0xff, 0x01,
]);

/**
 * Builder for configuring DPI and other sensor parameters of the Attack Shark X11 (and X3 variant).
 */
export class DpiBuilder implements BaseProtocolBuilder {
	public static readonly DEFAULT_OPTIONS: DpiBuilderOptions = {
		angleSnap: false,
		ripplerControl: true,
		dpiValues: [800, 1600, 2400, 3200, 5000, 22000],
		activeStage: 2,
	};

	/** Default options for the X3 variant (wired PID 0xfa61). */
	public static readonly X3_DEFAULT_OPTIONS: DpiBuilderOptions = {
		angleSnap: false,
		ripplerControl: false,
		lod: 1,
		motionSync: false,
		dpiValues: [800, 1600, 2400, 3200, 5000, 26000],
		activeStage: 2,
	};

	readonly buffer: Buffer;
	public readonly bmRequestType: number = 0x21;
	public readonly bRequest: number = 0x09;
	public readonly wValue: number = 0x0304;
	public readonly wIndex: number = 2;
	private stages: number[] = [800, 1600, 2400, 3200, 5000, 22000];
	private angleSnap = DpiBuilder.DEFAULT_OPTIONS.angleSnap!;
	private ripplerControl = DpiBuilder.DEFAULT_OPTIONS.ripplerControl!;
	private lod: 1 | 2 = 1;
	private motionSync = false;

	// noinspection FunctionTooLongJS
	constructor(options?: DpiBuilderOptions) {
		this.buffer = Buffer.alloc(56);

		this.buffer[0] = 0x04; // header
		this.buffer[1] = 0x38; // header
		this.buffer[2] = 0x01; // header

		this.buffer[OFFSET.ANGLE_SNAP] = 0x00; // angle snap
		this.buffer[OFFSET.RIPPLER_CONTROL] = 0x01; // ripple control

		this.buffer[OFFSET.STAGE_ENABLE_MASK] = 0x3f; // fixed

		this.buffer[OFFSET.STAGE_MASK_A] = 0x20; // stage mask
		this.buffer[OFFSET.STAGE_MASK_B] = 0x20; // stage mask

		this.buffer[8] = 0x12; // stage 1 value
		this.buffer[9] = 0x25; // stage 2 value
		this.buffer[10] = 0x38; // stage 3 value
		this.buffer[11] = 0x4b; // stage 4 value
		this.buffer[12] = 0x75; // stage 5 value
		this.buffer[13] = 0x81; // stage 6 value

		this.buffer[14] = 0x00; // fixed
		this.buffer[15] = 0x00; // fixed

		this.buffer[16] = 0x00; // high stage 1
		this.buffer[17] = 0x00; // high stage 2
		this.buffer[18] = 0x00; // high stage 3
		this.buffer[19] = 0x00; // high stage 4
		this.buffer[20] = 0x00; // high stage 5
		this.buffer[21] = 0x01; // high stage 6

		this.buffer[22] = 0x00; // fixed
		this.buffer[23] = 0x00; // fixed
		this.buffer[OFFSET.CURRENT_STAGE] = 0x02; // stage index
		for (let i = 0; i < X11_FIXED_BYTES.length; i++) {
			this.buffer[25 + i] = X11_FIXED_BYTES[i]!; // fixed
		}
		this.buffer[50] = 0x0f; // checksum high byte
		this.buffer[51] = 0x68; // checksum low byte

		this.buffer[52] = 0x00; // padding wireless mode
		this.buffer[53] = 0x00; // padding wireless mode
		this.buffer[54] = 0x00; // padding wireless mode
		this.buffer[55] = 0x00; // padding wireless mode

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

	/**
	 * Defines whether Angle Snapping (straight line correction) should be active.
	 * @param active True to activate. Default: false.
	 */
	public setAngleSnap(active: boolean = false): this {
		this.angleSnap = active;
		this.buffer[OFFSET.ANGLE_SNAP] = active ? 0x01 : 0x00;
		return this;
	}

	/**
	 * Defines whether Ripple Control (sensor noise smoothing) should be active.
	 * @param active True to activate. Default: true.
	 */
	public setRipplerControl(active: boolean = true): this {
		this.ripplerControl = active;
		this.buffer[OFFSET.RIPPLER_CONTROL] = active ? 0x01 : 0x00;
		return this;
	}

	/**
	 * Sets lift-off distance for X3 wired mode (1mm or 2mm).
	 * X11 modes do not encode this field in RID 04.
	 */
	public setLod(lod: 1 | 2 = 1): this {
		if (lod !== 1 && lod !== 2) {
			throw new ParamsError('lod', `LOD must be 1 or 2, got ${lod}`);
		}
		this.lod = lod;
		return this;
	}

	/**
	 * Defines whether Motion Sync should be active for X3 wired mode.
	 */
	public setMotionSync(active: boolean = false): this {
		this.motionSync = active;
		return this;
	}

	/**
	 * Defines which DPI stage is currently active (1 to 8 for X3, 1 to 6 for X11).
	 * @param stage Stage index (StageIndex).
	 */
	public setCurrentStage(stage: StageIndex): this {
		this.buffer[OFFSET.CURRENT_STAGE] = stage;
		return this;
	}

	/**
	 * Sets the DPI value for a specific stage.
	 * @param stage Stage index (1 to 8).
	 * @param dpi DPI value (must be supported by the sensor).
	 */
	public setDpiValue(stage: StageIndex, dpi: number): this {
		const index = stage - 1;

		if (stage > 6 && this.stages.length < stage) {
			this.stages.length = stage;
		}
		this.stages[index] = dpi;
		this.buffer[OFFSET.STAGES_START + index] = this.encodeDpi(dpi);

		return this;
	}

	/**
	 * Sets the DPI values for all provided stages.
	 *
	 * @param stages Array of 1 to 8 DPI values. Non-X3 modes still validate exactly 6 at build time.
	 * @return {this} The instance for method chaining.
	 */
	public setStages(stages: DpiStages): this {
		if (!Array.isArray(stages) || stages.length < 1 || stages.length > 8)
			throw new ParamsError(
				'stages',
				`You need to pass 1 to 8 DPI values; e.g.: [800, 1600, 2400, 3200, 5000, 22000]`,
			);

		this.stages = [...stages];
		for (let i = 0; i < Math.min(stages.length, 6); i++) {
			const dpi = stages[i]!;
			this.buffer[OFFSET.STAGES_START + i] = this.encodeDpi(dpi);
		}
		this.buffer[14] = 0x00;
		this.buffer[15] = 0x00;
		return this;
	}

	calculateChecksum(): number {
		let sum = 0;

		for (let i = 3; i <= 49; i++) {
			sum += this.buffer[i] ?? 0x00;
		}

		return sum & 0xffff;
	}

	/**
	 * Applies mode-specific fixed bytes at offsets 25-49.
	 * Wired / Adapter use X11 bytes; X3Wired uses X3 bytes.
	 * This makes repeated build() calls on the same builder deterministic
	 * regardless of the order of ConnectionMode values used.
	 */
	private applyFixedBytes(mode: ConnectionMode): void {
		const bytes = mode === ConnectionMode.X3Wired ? X3_FIXED_BYTES : X11_FIXED_BYTES;
		for (let i = 0; i < bytes.length; i++) {
			this.buffer[25 + i] = bytes[i]!;
		}
	}

	public build(mode: ConnectionMode): Buffer {
		this.applyFixedBytes(mode);
		this.applySensorBytes(mode);
		this.validateStageCount(mode);
		this.validateActiveStage(mode);
		this.validateDpiRange(mode);
		this.applyModeStageBytes(mode);
		if (mode !== ConnectionMode.X3Wired) {
			this.updateStageMask();
			this.updateHighStageFlags();
		}

		const checksum = this.calculateChecksum();
		this.buffer.writeUInt16BE(checksum, OFFSET.CHECKSUM_HIGH_BYTE);

		return isConnectionModeWired(mode) ? this.buffer.subarray(0, OFFSET.CHECKSUM_LOW_BYTE + 1) : this.buffer;
	}

	public toString(): string {
		return this.buffer.toString('hex');
	}

	public compareWithHexString(value: string): boolean {
		return this.toString() === value;
	}

	private encodeDpi(dpi: number): number {
		const keys = Object.keys(DPI_STEP_MAP)
			.map(Number)
			.sort((a, b) => a - b);

		const match = keys.find((k) => k >= dpi);

		if (match === undefined) {
			throw new ParamsError('dpi', `Unsupported DPI: ${dpi}`);
		}

		return DPI_STEP_MAP[match] ?? 0x00;
	}

	private encodeX3Dpi(dpi: number): { low: number; high: number } {
		if (!Number.isInteger(dpi) || dpi < 50 || dpi > 26000 || dpi % 50 !== 0) {
			throw new ParamsError('dpi', `X3 DPI must be an integer multiple of 50 between 50 and 26000, got ${dpi}`);
		}
		const raw = dpi / 50 - 1;
		return { low: raw & 0xff, high: raw >> 8 };
	}

	private applySensorBytes(mode: ConnectionMode): void {
		if (mode === ConnectionMode.X3Wired) {
			this.buffer[OFFSET.X3_LOD] = this.lod === 2 ? 0x01 : 0x00;
			this.buffer[OFFSET.RIPPLER_CONTROL] = this.ripplerControl ? 0x01 : 0x00;
			this.buffer[OFFSET.X3_ANGLE_SNAP] = this.angleSnap ? 0x01 : 0x00;
			this.buffer[OFFSET.X3_MOTION_SYNC] = this.motionSync ? 0x01 : 0x00;
			return;
		}

		this.buffer[OFFSET.ANGLE_SNAP] = this.angleSnap ? 0x01 : 0x00;
		this.buffer[OFFSET.RIPPLER_CONTROL] = this.ripplerControl ? 0x01 : 0x00;
	}

	private applyModeStageBytes(mode: ConnectionMode): void {
		if (mode === ConnectionMode.X3Wired) {
			for (let i = 0; i < 8; i++) {
				if (i >= this.stages.length) {
					this.buffer[OFFSET.STAGES_START + i] = 0x00;
					this.buffer[OFFSET.X3_STAGES_HIGH_START + i] = 0x00;
					continue;
				}

				const encoded = this.encodeX3Dpi(this.stages[i]!);
				this.buffer[OFFSET.STAGES_START + i] = encoded.low;
				this.buffer[OFFSET.X3_STAGES_HIGH_START + i] = encoded.high;
			}
			this.buffer[OFFSET.STAGE_ENABLE_MASK] = this.stages.length < 8 ? (1 << this.stages.length) - 1 : 0xff;
			return;
		}

		this.buffer[OFFSET.STAGE_ENABLE_MASK] = 0x3f;
		for (let i = 0; i < 6; i++) {
			this.buffer[OFFSET.STAGES_START + i] = this.encodeDpi(this.stages[i]!);
		}
		this.buffer[14] = 0x00;
		this.buffer[15] = 0x00;
		this.buffer[22] = 0x00;
		this.buffer[23] = 0x00;
	}

	private updateStageMask(): void {
		const stageCount = 6;
		let mask = 0x00;

		for (let i = 0; i < stageCount; i++) {
			// Mask bit is set if DPI is greater than 12,000
			if ((this.stages[i] ?? 0x00) > 12000) {
				mask |= 1 << i;
			}
		}

		this.buffer[OFFSET.STAGE_MASK_A] = mask;
		this.buffer[OFFSET.STAGE_MASK_B] = mask;
	}

	private updateHighStageFlags(): void {
		const upperLimit = 22000;
		for (let i = 0; i < this.stages.length; i++) {
			const dpi = this.stages[i] ?? 0x00;
			// Bytes 16-21 (High Stage Flags) are set to 0x01 if DPI is in range [10100, 12000] or [20100, upperLimit]
			if ((dpi >= 10100 && dpi <= 12000) || (dpi >= 20100 && dpi <= upperLimit)) {
				this.buffer[OFFSET.EXPANDED_MASK + i] = 0x01;
			} else {
				this.buffer[OFFSET.EXPANDED_MASK + i] = 0x00;
			}
		}
	}

	/**
	 * Mode-specific DPI-range validation.
	 * Non-X3 modes reject any stage above 22000. X3Wired allows up to 26000
	 * (values above 26000 are already blocked by {@link encodeDpi} via the DPI map).
	 */
	private validateDpiRange(mode: ConnectionMode): void {
		const maxDpi = mode === ConnectionMode.X3Wired ? 26000 : 22000;
		for (let i = 0; i < this.stages.length; i++) {
			const dpi = this.stages[i] ?? 0;
			if (dpi > maxDpi) {
				throw new ParamsError(
					'dpi',
					`Stage ${i + 1} DPI ${dpi} exceeds maximum ${maxDpi} for ${ConnectionMode[mode]}`,
				);
			}
		}
	}

	private validateStageCount(mode: ConnectionMode): void {
		if (mode !== ConnectionMode.X3Wired && this.stages.length !== 6) {
			throw new ParamsError('stages', 'X11 wired/adapter modes require exactly 6 DPI stages');
		}
	}

	private validateActiveStage(mode: ConnectionMode): void {
		const activeStage = this.buffer[OFFSET.CURRENT_STAGE] ?? 0;
		const maxStage = mode === ConnectionMode.X3Wired ? this.stages.length : 6;
		if (activeStage < 1 || activeStage > maxStage) {
			throw new ParamsError('activeStage', `Active stage ${activeStage} must be between 1 and ${maxStage}`);
		}
	}
}
