import * as fs from 'fs';
import * as os from 'os';
import * as path from 'path';

export type ServerSource = 'configuration' | 'workspace' | 'development' | 'bundle' | 'path';

export interface ServerResolution {
    readonly command: string;
    readonly source: ServerSource;
}

export interface ServerDiscoveryOptions {
    readonly configuredPath: string | null | undefined;
    readonly workspaceRoots: readonly string[];
    readonly extensionPath: string;
    readonly environment?: NodeJS.ProcessEnv;
    readonly platform?: NodeJS.Platform;
}

export interface ServerDiscoveryFailure {
    readonly message: string;
    readonly checked: readonly string[];
}

export type ServerDiscoveryResult =
    | { readonly resolution: ServerResolution; readonly failure?: never }
    | { readonly resolution?: never; readonly failure: ServerDiscoveryFailure };

export function discoverServer(options: ServerDiscoveryOptions): ServerDiscoveryResult {
    const platform = options.platform ?? process.platform;
    const environment = options.environment ?? process.env;
    const executable = platform === 'win32' ? 'arandu-lsp.exe' : 'arandu-lsp';
    const checked: string[] = [];

    const configured = options.configuredPath?.trim();
    if (configured) {
        const expanded = expandConfiguredPath(configured, options.workspaceRoots, environment);
        checked.push(expanded);
        if (!path.isAbsolute(expanded)) {
            return failure(
                'arandu.server.path must be an absolute path to the arandu-lsp executable.',
                checked
            );
        }
        if (isExecutableFile(expanded, platform)) {
            return success(expanded, 'configuration');
        }
        return failure(`Configured Arandu language server was not executable: ${expanded}`, checked);
    }

    for (const root of options.workspaceRoots) {
        for (const profile of ['release', 'debug'] as const) {
            const candidate = path.join(root, 'target', profile, executable);
            checked.push(candidate);
            if (isExecutableFile(candidate, platform)) {
                return success(candidate, 'workspace');
            }
        }
    }

    const repositoryRoot = path.resolve(options.extensionPath, '..', '..');
    for (const profile of ['release', 'debug'] as const) {
        const candidate = path.join(repositoryRoot, 'target', profile, executable);
        checked.push(candidate);
        if (isExecutableFile(candidate, platform)) {
            return success(candidate, 'development');
        }
    }

    const bundled = findBundledServer(options.extensionPath, platform, checked);
    if (bundled) {
        return success(bundled, 'bundle');
    }

    const fromPath = findOnPath(executable, environment, platform, checked);
    if (fromPath) {
        return success(fromPath, 'path');
    }

    return failure(
        'Could not find an arandu-lsp server. Checked the configured path, the ' +
            'workspace/repository target builds, the server bundled in this VSIX, and the PATH. ' +
            'Install the Arandu SDK or set arandu.server.path.',
        checked
    );
}

function findBundledServer(
    extensionPath: string,
    platform: NodeJS.Platform,
    checked: string[]
): string | undefined {
    const suffixed = platform === 'win32' ? 'arandu-lsp-win32.exe' : `arandu-lsp-${platform}`;
    const plain = platform === 'win32' ? 'arandu-lsp.exe' : 'arandu-lsp';
    for (const name of [suffixed, plain]) {
        const candidate = path.join(extensionPath, 'bin', name);
        checked.push(candidate);
        if (isExecutableFile(candidate, platform)) {
            return candidate;
        }
    }
    return undefined;
}

function expandConfiguredPath(
    configured: string,
    workspaceRoots: readonly string[],
    environment: NodeJS.ProcessEnv
): string {
    let expanded = configured;
    if (expanded === '~' || expanded.startsWith(`~${path.sep}`)) {
        expanded = path.join(os.homedir(), expanded.slice(2));
    }
    if (workspaceRoots[0]) {
        expanded = expanded.replaceAll('${workspaceFolder}', workspaceRoots[0]);
    }
    expanded = expanded.replaceAll(/\$\{env:([^}]+)\}/g, (_match, name: string) => environment[name] ?? '');
    return path.normalize(expanded);
}

function findOnPath(
    executable: string,
    environment: NodeJS.ProcessEnv,
    platform: NodeJS.Platform,
    checked: string[]
): string | undefined {
    const pathValue = environment.PATH ?? environment.Path ?? environment.path ?? '';
    const names = platform === 'win32'
        ? windowsExecutableNames(executable, environment.PATHEXT)
        : [executable];
    for (const directory of pathValue.split(path.delimiter).filter(Boolean)) {
        for (const name of names) {
            const candidate = path.join(directory, name);
            checked.push(candidate);
            if (isExecutableFile(candidate, platform)) {
                return candidate;
            }
        }
    }
    return undefined;
}

function windowsExecutableNames(executable: string, pathExt: string | undefined): string[] {
    if (path.extname(executable)) {
        return [executable];
    }
    const extensions = (pathExt ?? '.EXE;.CMD;.BAT;.COM').split(';').filter(Boolean);
    return extensions.map(extension => `${executable}${extension.toLowerCase()}`);
}

function isExecutableFile(candidate: string, platform: NodeJS.Platform): boolean {
    try {
        if (!fs.statSync(candidate).isFile()) {
            return false;
        }
        fs.accessSync(candidate, platform === 'win32' ? fs.constants.F_OK : fs.constants.X_OK);
        return true;
    } catch {
        return false;
    }
}

function success(command: string, source: ServerSource): ServerDiscoveryResult {
    return { resolution: { command, source } };
}

function failure(message: string, checked: readonly string[]): ServerDiscoveryResult {
    return { failure: { message, checked } };
}
