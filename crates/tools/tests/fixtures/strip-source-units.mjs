import { readFileSync } from 'node:fs';
import { stripTypeScriptTypes } from 'node:module';

const prefix = 'async function __dsh_program__() {\n';
const suffix = '\n}';
const programs = JSON.parse(readFileSync(0, 'utf8'));
const units = value => Array.from({ length: value.length }, (_, index) => value.charCodeAt(index));
const records = programs.map(program => {
  try {
    const stripped = stripTypeScriptTypes(prefix + program + suffix);
    const body = stripped.slice(prefix.length, stripped.length - suffix.length);
    return { input: program, inputUnits: units(program), body, bodyUnits: units(body) };
  } catch (error) {
    return { input: program, inputUnits: units(program), error: { name: error.name, code: error.code, message: error.message } };
  }
});
process.stdout.write(JSON.stringify({ node: process.version, amaro: process.versions.amaro, records }));
