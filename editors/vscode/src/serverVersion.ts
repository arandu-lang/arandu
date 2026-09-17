import * as childProcess from 'child_process';

export interface ServerVersionCheck {
    readonly ok: boolean;
    readonly output: string;
}

const VERSION_TIMEOUT_MS = 10_000;

// The resolved command must identify itself as `arandu-lsp` when run with
// `--version`; a stale, damaged or unrelated binary is rejected before the
// language client starts, mirroring the rust-analyzer server validation.
export function checkServerVersion(command: string): Promise<ServerVersionCheck> {
    return new Promise(resolve => {
        const child = childProcess.spawn(command, ['--version'], {
            windowsHide: true,
            stdio: ['ignore', 'pipe', 'pipe']
        });
        let output = '';
        child.stdout?.on('data', chunk => {
            output += String(chunk);
        });
        child.stderr?.on('data', chunk => {
            output += String(chunk);
        });
        const timer = setTimeout(() => {
            output += '\n[--version timed out]';
            child.kill('SIGKILL');
            resolve({ ok: false, output });
        }, VERSION_TIMEOUT_MS);
        child.on('error', () => {
            clearTimeout(timer);
            resolve({ ok: false, output });
        });
        child.on('close', code => {
            clearTimeout(timer);
            resolve({ ok: code === 0 && /arandu-lsp/i.test(output), output });
        });
    });
}