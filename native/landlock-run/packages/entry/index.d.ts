export declare const LAUNCHER_BIN = "landlock-run";
export declare const LAUNCHER_FAILURE_EXIT = 125;
export type LandlockEnforcement = 'full' | 'partial' | 'unusable';
export interface LauncherGrants {
  readonly readOnly?: readonly string[];
  readonly readWrite?: readonly string[];
}
export declare function launcherPath(resolvePackageJson?: (specifier: string) => string): string;
export declare function grantArgs(grants: LauncherGrants): string[];
export declare function probe(launcher?: string, options?: { timeoutMs?: number }): LandlockEnforcement;
