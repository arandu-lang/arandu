import * as assert from 'node:assert/strict';
import * as fs from 'node:fs';
import * as path from 'node:path';
import { test } from 'node:test';

void test('TextMate grammar delegates colors to the active theme', () => {
    const grammarPath = path.resolve(__dirname, '..', '..', '..', 'syntaxes', 'arandu.tmLanguage.json');
    const grammar = fs.readFileSync(grammarPath, 'utf8');

    assert.doesNotMatch(grammar, /"(?:foreground|background)"\s*:/u);
    for (const scope of [
        'comment.line.double-slash.arandu',
        'string.quoted.double.arandu',
        'keyword.control.arandu',
        'constant.numeric.dec.arandu',
        'entity.name.function.arandu',
        'entity.name.tag.annotation.arandu',
        'entity.name.type.arandu',
        'variable.other.arandu'
    ]) {
        assert.match(grammar, new RegExp(scope.replaceAll('.', '\\.'), 'u'));
    }
});

void test('TextMate grammar highlights current Arandu keywords and primitive types', () => {
    const grammarPath = path.resolve(__dirname, '..', '..', '..', 'syntaxes', 'arandu.tmLanguage.json');
    const grammar = JSON.parse(fs.readFileSync(grammarPath, 'utf8')) as {
        repository?: Record<string, { patterns?: Array<{ match?: string }> }>;
    };
    const keywordPatterns = grammar.repository?.keywords?.patterns ?? [];
    const primitivePatterns = grammar.repository?.types?.patterns ?? [];
    const keywordRegex = keywordPatterns.map(pattern => pattern.match ?? '').join('|');
    const typeRegex = primitivePatterns.map(pattern => pattern.match ?? '').join('|');

    for (const keyword of ['impl', 'internal', 'private', 'ptr', 'alloc', 'free', 'comptime']) {
        assert.match(keywordRegex, new RegExp(`\\b${keyword}\\b`, 'u'));
    }
    for (const type of ['void', 'isize', 'usize']) {
        assert.match(typeRegex, new RegExp(`\\b${type}\\b`, 'u'));
    }
});

void test('block indentation includes impl, defer, and unsafe bodies', () => {
    const configPath = path.resolve(__dirname, '..', '..', '..', 'language-configuration.json');
    const config = JSON.parse(fs.readFileSync(configPath, 'utf8')) as {
        indentationRules?: { increaseIndentPattern?: string };
    };
    const increase = new RegExp(config.indentationRules?.increaseIndentPattern ?? '', 'u');

    for (const line of ['impl Printable for Item {', 'defer {', 'unsafe {', 'comptime {', 'let answer = comptime {', 'return comptime {']) {
        assert.match(line, increase, `Enter after '${line}' should indent the next line`);
    }
});

void test('layout expressions use the builtin function scope before annotation fallback', () => {
    const grammarPath = path.resolve(__dirname, '..', '..', '..', 'syntaxes', 'arandu.tmLanguage.json');
    const grammar = JSON.parse(fs.readFileSync(grammarPath, 'utf8')) as {
        repository: Record<string, { patterns: Array<{ name: string; match: string }> }>;
    };
    const patterns = grammar.repository.decorators.patterns;
    assert.equal(patterns[0].name, 'support.function.builtin.arandu');
    const intrinsic = new RegExp(patterns[0].match, 'u');
    for (const expression of ['@sizeOf(int)', '@alignOf([3]u16)']) {
        assert.match(expression, intrinsic);
    }
    for (const annotation of ['@test', '@Test', '@sizeOfSomething(int)']) {
        assert.doesNotMatch(annotation, intrinsic);
    }
});

void test('annotation semantic tokens use the TextMate annotation fallback', () => {
    const manifestPath = path.resolve(__dirname, '..', '..', '..', 'package.json');
    const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8')) as {
        contributes?: {
            semanticTokenScopes?: Array<{
                scopes?: Record<string, string[]>;
            }>;
        };
    };
    const scopes = manifest.contributes?.semanticTokenScopes?.[0]?.scopes;
    assert.deepEqual(scopes?.decorator, ['entity.name.tag.annotation.arandu']);
});

void test('format on save is opt-in and Arandu owns its manual formatter', () => {
    const manifestPath = path.resolve(__dirname, '..', '..', '..', 'package.json');
    const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8')) as {
        contributes?: {
            configurationDefaults?: Record<string, Record<string, unknown>>;
        };
    };
    const defaults = manifest.contributes?.configurationDefaults?.['[arandu]'];
    assert.equal(defaults?.['editor.defaultFormatter'], 'arandu.arandu-lang');
    assert.equal(defaults?.['editor.formatOnSave'], false);
});

void test('arandu snippets are registered in manifest and define core constructs', () => {
    const root = path.resolve(__dirname, '..', '..', '..');
    const manifestPath = path.join(root, 'package.json');
    const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8')) as {
        contributes?: {
            snippets?: Array<{
                language: string;
                path: string;
            }>;
        };
    };
    const snippetsEntry = manifest.contributes?.snippets?.find((s) => s.language === 'arandu');
    assert.ok(snippetsEntry, 'snippets entry for arandu must be declared in package.json');
    assert.equal(snippetsEntry.path, './snippets/arandu.json');

    const snippetsFilePath = path.resolve(root, snippetsEntry.path);
    assert.ok(fs.existsSync(snippetsFilePath), 'snippets/arandu.json must exist');

    const snippets = JSON.parse(fs.readFileSync(snippetsFilePath, 'utf8')) as Record<
        string,
        { prefix: string | string[]; body: string | string[]; description?: string }
    >;

    for (const expectedKey of [
        'Main Function (int)',
        'Function Declaration',
        'Let Variable',
        'While Loop',
        'If Condition',
        'Struct Declaration',
        'Enum Declaration',
        'Print Line'
    ]) {
        assert.ok(snippets[expectedKey], `Snippet '${expectedKey}' must be present in snippets/arandu.json`);
    }
    const testBody = snippets['Test Function']?.body;
    assert.ok(Array.isArray(testBody));
    assert.equal(testBody[0], '@Test', 'the test snippet must use the compiler-recognized attribute');
});

void test('package initialization is discoverable and manifest workspaces activate the extension', () => {
    const manifestPath = path.resolve(__dirname, '..', '..', '..', 'package.json');
    const manifest = JSON.parse(fs.readFileSync(manifestPath, 'utf8')) as {
        activationEvents?: string[];
        contributes?: { commands?: Array<{ command: string; title: string }> };
    };
    assert.ok(manifest.activationEvents?.includes('workspaceContains:**/arandu.toml'));
    assert.ok(manifest.activationEvents?.includes('workspaceContains:**/Arandu.toml'));
    assert.ok(manifest.contributes?.commands?.some(command =>
        command.command === 'arandu.initializePackage' && command.title === 'Arandu: Initialize Package'));
});
