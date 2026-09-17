import * as assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as os from 'node:os';
import * as path from 'node:path';
import { afterEach, test } from 'node:test';
import { checkServerVersion } from '../../serverVersion';

const fixtures: string[] = [];

afterEach((): void => {
    for (const fixture of fixtures.splice(0)) {
        fs.rmSync(fixture, { recursive: true, force: true });
    }
});

// Spawning an executable requires a real launcher; on Windows drag that
// through shell scripting is not worth a product change, so the version gate
// itself is exercised by the Extension Host suite on the real binary.
const spawnSupported = process.platform !== 'win32';

void test('a server that identifies itself on --version passes the gate', async (): Promise<void> => {
    if (!spawnSupported) {
        return;
    }
    const command = writeScript('#!/bin/sh\nprintf "%s\\n" "arandu-lsp 0.1.0-rc.6"\n');
    const check = await checkServerVersion(command);
    assert.equal(check.ok, true);
    assert.match(check.output, /arandu-lsp/);
});

void test('an executable without arandu-lsp output is rejected', async (): Promise<void> => {
    if (!spawnSupported) {
        return;
    }
    const command = writeScript('#!/bin/sh\nprintf "%s\\n" "some other tool"\n');
    const check = await checkServerVersion(command);
    assert.equal(check.ok, false);
});

void test('a failing --version exit is rejected even with plausible output', async (): Promise<void> => {
    if (!spawnSupported) {
        return;
    }
    const command = writeScript('#!/bin/sh\nprintf "%s\\n" "arandu-lsp 0.1.0-rc.6"\nexit 3\n');
    const check = await checkServerVersion(command);
    assert.equal(check.ok, false);
});

void test('a missing executable is rejected without hanging', async (): Promise<void> => {
    if (!spawnSupported) {
        return;
    }
    const command = path.join(fixture(), 'does-not-exist');
    const check = await checkServerVersion(command);
    assert.equal(check.ok, false);
});

function writeScript(content: string): string {
    const directory = fixture();
    const file = path.join(directory, 'fake-server.sh');
    fs.writeFileSync(file, content);
    fs.chmodSync(file, 0o755);
    return file;
}

function fixture(): string {
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'arandu-version-test-'));
    fixtures.push(directory);
    return directory;
}