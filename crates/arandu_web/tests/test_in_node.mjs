import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const __dirname = dirname(fileURLToPath(import.meta.url));
const wasmPath = join(__dirname, '../../../target/wasm32-unknown-unknown/release/arandu_web.wasm');
const wasmBytes = await readFile(wasmPath);

const { instance } = await WebAssembly.instantiate(wasmBytes, {});
const memory = instance.exports.memory;
const { arandu_alloc, arandu_free, arandu_compile, arandu_free_response, arandu_format, arandu_free_json } = instance.exports;

function compile(source) {
  const enc = new TextEncoder();
  const sourceBytes = enc.encode(source);
  const ptr = arandu_alloc(sourceBytes.length);
  new Uint8Array(memory.buffer, ptr, sourceBytes.length).set(sourceBytes);

  const respPtr = arandu_compile(ptr, sourceBytes.length);
  const view = new DataView(memory.buffer, respPtr, 20);
  const success = view.getUint32(0, true) === 1;
  const wasmPtr = view.getUint32(4, true);
  const wasmLen = view.getUint32(8, true);
  const jsonPtr = view.getUint32(12, true);
  const jsonLen = view.getUint32(16, true);

  let outputWasm = null;
  if (success && wasmPtr !== 0 && wasmLen > 0) {
    outputWasm = new Uint8Array(memory.buffer, wasmPtr, wasmLen).slice();
  }

  let diagnostics = [];
  if (jsonPtr !== 0 && jsonLen > 0) {
    const jsonStr = new TextDecoder().decode(new Uint8Array(memory.buffer, jsonPtr, jsonLen));
    diagnostics = JSON.parse(jsonStr);
  }

  arandu_free_response(respPtr);
  arandu_free(ptr, sourceBytes.length);
  return { success, outputWasm, diagnostics };
}

function format(source) {
  const enc = new TextEncoder();
  const sourceBytes = enc.encode(source);
  const ptr = arandu_alloc(sourceBytes.length);
  new Uint8Array(memory.buffer, ptr, sourceBytes.length).set(sourceBytes);

  const respPtr = arandu_format(ptr, sourceBytes.length);
  const view = new DataView(memory.buffer, respPtr, 8);
  const jsonPtr = view.getUint32(0, true);
  const jsonLen = view.getUint32(4, true);

  const jsonStr = new TextDecoder().decode(new Uint8Array(memory.buffer, jsonPtr, jsonLen));
  const formatted = JSON.parse(jsonStr);

  arandu_free_json(respPtr);
  arandu_free(ptr, sourceBytes.length);
  return formatted;
}

console.log('Testing in-browser formatter WebAssembly...');
const unformatted = 'public func main():i32{return 42;}';
const formatted = format(unformatted);
console.log('Formatted code:\n' + formatted);
if (!formatted.includes('public func main(): i32')) {
  console.error('Formatting failed!');
  process.exit(1);
}

console.log('\nTesting in-browser compiler WebAssembly with std.core...');
const coreProgram = `
import std.core.fixed as fixed

public func main(): i32 {
    let one = fixed.fromInt(1 as i16)
    let two = fixed.fromInt(2 as i16)
    let sum = one.add(two)
    return sum.toInt()
}
`;
const coreRes = compile(coreProgram);
console.log('std.core compile success:', coreRes.success);
if (!coreRes.success || !coreRes.outputWasm) {
  console.error('std.core compilation failed:', coreRes.diagnostics);
  process.exit(1);
}

const guestCoreModule = await WebAssembly.instantiate(coreRes.outputWasm, {});
const coreRet = guestCoreModule.instance.exports.main();
console.log('std.core Q16.16 1 + 2 =', coreRet);
if (coreRet !== 3) {
  console.error('std.core calculation assertion failed: expected 3, got', coreRet);
  process.exit(1);
}

console.log('\nTesting in-browser compiler WebAssembly with host I/O...');
const program = `
extern "env" {
    func print_str(msg: str): void
}

public func main(): i32 {
    unsafe {
        print_str("Olá do Arandu via WebAssembly no Navegador!")
    }
    return 100
}
`;

const res = compile(program);
console.log('Compilation success:', res.success);
console.log('Emitted wasm bytes:', res.outputWasm?.length);
console.log('Diagnostics:', res.diagnostics);

if (!res.success || !res.outputWasm) {
  console.error('Compilation failed!');
  process.exit(1);
}

const mod = new WebAssembly.Module(res.outputWasm);
console.log('Guest module imports:', WebAssembly.Module.imports(mod));
console.log('Guest module exports:', WebAssembly.Module.exports(mod));

// Now instantiate and RUN the compiled program!
let guestMemory;
let printedText = '';
const guestImports = {
  env: {
    print_str: (ptr, len) => {
      const bytes = new Uint8Array(guestMemory.buffer, ptr, len);
      printedText += new TextDecoder().decode(bytes) + '\n';
    }
  }
};

const guestModule = await WebAssembly.instantiate(res.outputWasm, guestImports);
guestMemory = guestModule.instance.exports.memory;
const retCode = guestModule.instance.exports.main();
console.log('Guest execution return code:', retCode);
console.log('Guest printed output:');
process.stdout.write(printedText);

if (retCode === 100 && printedText.includes('Olá do Arandu')) {
  console.log('\n>>> SUCCESS! In-browser compilation, formatting, and execution with std.core works 100%! <<<');
} else {
  console.error('Execution assertion failed: expected 100, got', retCode);
  process.exit(1);
}
