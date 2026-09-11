import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath, URL } from 'node:url';
import rust from './seekdeep_landlock_entry.cjs';

rust.configureBindings({ process, resolve: createRequire(import.meta.url).resolve, dirname, join, fileURLToPath, URL, moduleUrl: import.meta.url, spawnSync });

export const LAUNCHER_BIN = rust.launcherBin();
export const LAUNCHER_FAILURE_EXIT = rust.launcherFailureExit();
export function launcherPath(resolvePackageJson = undefined) {
  return rust.launcherPath(resolvePackageJson);
}
export const grantArgs = rust.grantArgs;
export function probe(launcher = undefined, options = undefined) {
  return rust.probe(launcher, options);
}
