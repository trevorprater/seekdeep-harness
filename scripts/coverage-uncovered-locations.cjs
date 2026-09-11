'use strict';

const { execFileSync } = require('node:child_process');
const { existsSync } = require('node:fs');
const path = require('node:path');
const { ReportBase } = require('istanbul-lib-report');

function reportBinary() {
  if (process.env.SEEKDEEP_COVERAGE_REPORT_BIN) return process.env.SEEKDEEP_COVERAGE_REPORT_BIN;
  const target = process.env.CARGO_TARGET_DIR
    ? path.resolve(process.env.CARGO_TARGET_DIR)
    : path.join(__dirname, '../target');
  const binary = path.join(target, process.env.CARGO_BUILD_TARGET || '', 'debug',
    `coverage-uncovered-locations${process.platform === 'win32' ? '.exe' : ''}`);
  if (!existsSync(binary)) {
    throw new Error(`coverage-uncovered-locations: compiled Rust reporter not found at ${binary}. Run \`cargo build -p seekdeep-repository-tools --bin coverage-uncovered-locations\` or set SEEKDEEP_COVERAGE_REPORT_BIN to the compiled executable.`);
  }
  return binary;
}

function native(request) {
  const input = JSON.stringify(request, (_key, value) => typeof value === 'number' && !Number.isFinite(value)
    ? { $seekdeepNumber: String(value) }
    : value);
  return JSON.parse(execFileSync(reportBinary(), { input, encoding: 'utf8', maxBuffer: Infinity }));
}

function reportState(report) {
  return { projectRoot: report.projectRoot, records: report.records };
}

class UncoveredLocationsReport extends ReportBase {
  constructor(opts = {}) {
    super(opts);
    Object.assign(this, native({ operation: 'create', projectRoot: opts.projectRoot, cwd: process.cwd() }));
  }

  onStart() {
    this.records = native({ operation: 'start', state: reportState(this) }).records;
  }

  onDetail(node) {
    const state = native({ operation: 'detail', state: reportState(this), coverage: node.getFileCoverage() });
    for (const record of state.records.slice(this.records.length)) this.records.push(record);
  }

  onEnd() {
    for (const line of native({ operation: 'end', state: reportState(this) })) console.log(line);
  }
}

module.exports = UncoveredLocationsReport;
