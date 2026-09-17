import * as assert from 'node:assert/strict';
import { test } from 'node:test';
import { FlatRunnableSymbol, planRunnables } from '../../runnablePlanner';

// vscode.SymbolKind.Function as the client re-enumerates LSP kinds (11, not 12).
const FUNCTION = 11;

const main: FlatRunnableSymbol = { name: 'main', kind: FUNCTION, line: 0 };
const testFn: FlatRunnableSymbol = { name: 'editorDiscoversThisTest', kind: FUNCTION, line: 3 };
const helper: FlatRunnableSymbol = { name: 'add', kind: FUNCTION, line: 5 };
const constant: FlatRunnableSymbol = { name: 'VERSION', kind: 13, line: 9 };

void test('main gains Run and Check lenses above its line', (): void => {
    const planned = planRunnables([main, constant], FUNCTION, () => false);
    assert.deepEqual(planned, [
        { line: 0, action: 'run', name: 'main' },
        { line: 0, action: 'check', name: 'main' }
    ]);
});

void test('discovered test functions gain only a Run Test lens', (): void => {
    const isTest = (name: string): boolean => name === 'editorDiscoversThisTest';
    const planned = planRunnables([testFn, helper, constant], FUNCTION, isTest);
    assert.deepEqual(planned, [
        { line: 3, action: 'test', name: 'editorDiscoversThisTest' }
    ]);
});

void test('a main that is also a test yields Run Check and Run Test exactly once', (): void => {
    const planned = planRunnables([main, main], FUNCTION, () => true);
    assert.deepEqual(planned, [
        { line: 0, action: 'run', name: 'main' },
        { line: 0, action: 'check', name: 'main' },
        { line: 0, action: 'test', name: 'main' }
    ]);
});

void test('non-function symbols never produce runnables', (): void => {
    assert.deepEqual(planRunnables([constant, helper], FUNCTION, () => true), [
        { line: 5, action: 'test', name: 'add' }
    ]);
});