# RFC 0019: Arquitetura de Cache Global Compartilhado, Content-Addressable Storage (CAS) e Prevenção de Inchaço de Build (*Zero-Bloat Target & Storage Management*)

- **Número da RFC:** 0019
- **Título:** Arquitetura de Cache Global Compartilhado, Content-Addressable Storage (CAS) e Prevenção de Inchaço de Build (*Zero-Bloat Target & Storage Management*)
- **Autor(es):** Equipe do Compilador Arandu
- **Data de Início:** 2026-09-19
- **Status:** `Draft`
- **Área Principal:** `Tooling` / `Build` / `Storage` (`arandu_cli`, `arandu_query`, `arandu_codegen`)
- **Documentos Relacionados:**
  - `docs/arandu-project-package-lifecycle-gold-v0.1.md`
  - `docs/arandu-compiler-roadmap-v0.1.md`
  - `docs/rfcs/0004-project-package-lifecycle.md`
  - `docs/rfcs/0011-incremental-partitioned-aot-and-in-process-linker.md`
  - `docs/rfcs/0013-deterministic-ctfe-and-comptime-metaprogramming.md`

---

## 1. Resumo (Summary)

Esta RFC define a arquitetura oficial de armazenamento de artefatos, gestão de compilação incremental e política de integridade de disco do Arandu, erradicando o problema crônico de explosão do diretório de build que afeta o ecossistema Rust (onde pastas `target/` acumulam de 10 GB a mais de 50 GB por projeto).

A proposta substitui a herança do modelo isolado do Cargo por uma arquitetura em 5 pilares:
1. **Diretório `target/` Local Estritamente Limpo**: A pasta `target/` na raiz do projeto armazena **exclusivamente os produtos finais solicitados** (`bin/` e `lib/`). Arquivos de objetos intermediários (`.o`), CGUs, metadados `.amir`, `.air` e árvores incrementais são permanentemente banidos da pasta local do projeto.
2. **Content-Addressable Storage (CAS) Global Compartilhado**: Todos os artefatos intermediários residem em um repositório centralizado do usuário (`~/.cache/arandu/cas/`), indexados pelo digest criptográfico **BLAKE3** de suas entradas semânticas exatas. Módulos da biblioteca padrão e dependências compartilhadas por múltiplos projetos são compilados **exatamente uma vez** na máquina.
3. **Coletor de Lixo com Teto Rígido de Disco (*Disk Budget Controller*)**: O compilador impõe um teto de armazenamento configurável (padrão: **2,0 GiB**). Ao final de cada compilação, um mecanismo de auto-limpeza em segundo plano expurga automaticamente os artefatos menos recentemente utilizados (**LRU - Least Recently Used**). O desenvolvedor nunca precisa executar `arandu clean` nem instalar ferramentas de terceiros como `cargo-sweep`.
4. **Política de Depuração Leve (*Line-Tables-First & Split DWARF*)**: Em compilações de desenvolvimento (`debug`), o compilador emite apenas tabelas de linhas de execução para rastreamento de pilha (*stack traces*), eliminando 75% a 85% do inchaço tradicional de metadados DWARF. Metadados completos de depuração são emitidos em arquivos desacoplados (*Split DWARF* `.dwo`/`.dwp`).
5. **Publicação Instantânea Zero-Copy via Reflinks (Copy-on-Write)**: A cópia do binário final do CAS para o `target/` local utiliza primitivas de clone de blocos de sistemas de arquivos modernos (`ioctl(FICLONE)` no Linux, `clonefile` no macOS APFS e `FSCTL_DUPLICATE_EXTENTS_TO_FILE` no Windows ReFS), alcançando publicação em sub-milissegundo com **zero bytes adicionais de espaço físico em disco**.

---

## 2. Motivação (Motivation)

### 2.1 A Crise do Espaço em Disco no Ecossistema Rust/Cargo

No ecossistema Rust, o Cargo implementou um modelo de isolamento absoluto por workspace. Embora conceitualmente simples, essa decisão gera consequências desastrosas no uso real:

1. **Duplicação Maciça entre Projetos**: Se um desenvolvedor possui 10 projetos em seu computador e todos utilizam crates comuns (como `tokio`, `serde`, `clap` ou `axum`), cada projeto compila sua própria cópia idêntica. Cada árvore de dependências consome de 2 a 5 GB, totalizando **30 a 50 GB de desperdício em arquivos binários duplicados**.
2. **Inchaço Descontrolado de Metadados DWARF**: Em builds normais de desenvolvimento, as tabelas de tipos e expansões DWARF representam até 85% do tamanho de cada arquivo `.o` e `.rlib`. A monomorfização multiplica esses dados por cada unidade de compilação (CGU).
3. **Acúmulo Eterno de Sessões Mortas**: O compilador não possui rotina de limpeza (*garbage collector*). Se o usuário altera uma flag do compilador ou atualiza a toolchain, as sessões de compilação incremental anteriores em `target/debug/incremental/` tornam-se órfãs e permanecem no disco para sempre.
4. **Fricção Cognitiva**: Desenvolvedores são forçados a gerenciar manualmente scripts de limpeza periódica (`cargo clean`, `cargo-sweep`, `cargo-cache`) ou esgotam repentinamente o espaço de armazenamento de seus SSDs durante esteiras de integração contínua (CI).

### 2.2 O Erro Herdados nos Primeiros Rascunhos do Arandu

No documento preliminar [`docs/arandu-project-package-lifecycle-gold-v0.1.md`](file:///home/bruno/Documentos/Desenvolvimento/Arandu-Lang/docs/arandu-project-package-lifecycle-gold-v0.1.md#L180-L185), o Arandu havia herdado provisoriamente o layout de pastas do Cargo:
```text
target/<profile>/<target-triple>/bin/
target/<profile>/<target-triple>/deps/          <-- Lixo intermediário duplicado por projeto
target/<profile>/<target-triple>/incremental/   <-- Sessões incrementais acumulando sem teto
```
Se esse contrato fosse mantido na versão final, o compilador Arandu incorreria exatamente no mesmo problema que aflige o Rust. Esta RFC revoga esse layout e formaliza o modelo de **Pegada Zero**.

---

## 3. Explicação em Nível de Guia (Guide-Level Explanation)

### 3.1 A Nova Experiência do Desenvolvedor

Para o desenvolvedor Arandu, a pasta do seu projeto permanece cirurgicamente limpa:

```text
meu_projeto/
 ├── arandu.toml
 ├── arandu.lock
 ├── src/
 │    └─ main.aru
 └── target/
      └─ debug/
          └─ meu_app          ◄── APENAS O EXECUTÁVEL FINAL! (~10 MB a 20 MB)
```

Nenhum arquivo temporário `.o`, nenhuma pasta `deps/`, nenhum arquivo `.amir` ou lixo de compilação fica exposto na pasta do projeto. Se o usuário quiser compartilhar o projeto, compactar um zip ou inspecionar o diretório, o tamanho total da pasta do projeto é de apenas **alguns kilobytes de código fonte mais o executável gerado**.

### 3.2 Onde Fica o Cache e Como Ele é Compartilhado?

Todos os objetos intermediários residem no diretório de cache do usuário:
* **Linux / BSD**: `~/.cache/arandu/cas/`
* **macOS**: `~/Library/Caches/arandu/cas/`
* **Windows**: `%LOCALAPPDATA%\arandu\cache\cas\`

Quando o desenvolvedor compila `meu_projeto_1` e depois cria `meu_projeto_2`:
* A biblioteca padrão (`std.core`, `std.alloc`, `std.fs`) é **reutilizada instantaneamente do cache global** sem ser recompilada;
* O tempo de compilação do segundo projeto cai de 400 ms para **30 ms**;
* O espaço ocupado no SSD para o segundo projeto é praticamente **zero**.

### 3.3 Gestão Automática de Teto de Disco (*Zero Maintenance*)

O desenvolvedor nunca precisa limpar o cache manualmente. O compilador gerencia o disco de forma autônoma:

```bash
# Consultar o estado do cache e espaço utilizado:
$ arandu cache status
Cache Global CAS: 1.42 GiB / 2.00 GiB (71% utilizado)
Artefatos em cache: 1.842 objetos (CGUs, módulos, metadados)
Última limpeza automática: há 2 horas (liberados 380 MiB via LRU)

# Ajustar o limite de disco (opcional):
$ arandu config set cache.max_size 4G
```

Se uma compilação fizer o cache ultrapassar o teto estipulado (ex: 2.0 GiB), o compilador aciona silenciosamente em background o coletor de lixo, expurgando as entradas mais antigas até retornar a 80% do teto.

---

## 4. Explicação em Nível de Referência (Reference-Level Explanation)

### 4.1 Estrutura do Repositório Global CAS

O Content-Addressable Storage do Arandu é organizado por prefixo de hash criptográfico para evitar sobrecarga de diretórios no sistema de arquivos:

```text
~/.cache/arandu/
 ├── cas/
 │    ├── 0a/
 │    │    └─ 0a3f8c... (Artefato binário envelope ARANCAS\0)
 │    ├── 8f/
 │    │    └─ 8fc12e...
 │    └─ ...
 ├── index/
 │    └─ catalog.db (SQLite / sled / lmdb mínimo rastreando atime, tamanho e hashes)
 └── locks/
      └─ ... (Flock atômico para sincronização multiprocesso segura)
```

### 4.2 O Envelope Canônico de Artefato (`ARANCAS\0`)

Conforme introduzido em `crates/arandu_query/src/artifact_cache.rs`, todo artefato salvo no CAS é encapsulado em um envelope autocontido e verificável:

```text
┌─────────────────────────┬────────────────────────────────────────────────┐
│ Offset / Tamanho        │ Conteúdo                                       │
├─────────────────────────┼────────────────────────────────────────────────┤
│ 0..8 (8 bytes)          │ Magic Header: *b"ARANCAS\0"                    │
│ 8..10 (2 bytes)         │ Schema Version: ARTIFACT_SCHEMA_VERSION (u16)  │
│ 10..11 (1 byte)         │ Artifact Kind: Air=1, Amir=2, Ameta=3, Cgu=4   │
│ 11..12 (1 byte)         │ Target Pointer Width: 2 (16b), 4 (32b), 8 (64b)│
│ 12..44 (32 bytes)       │ BLAKE3 Digest do Conteúdo Útil                 │
│ 44..52 (8 bytes)        │ Timestamp de Criação / mtime                   │
│ 52..88 (36 bytes)       │ Metadados de Toolchain & TargetInfo Flags      │
│ 88..EOF                 │ Payload do Artefato (Bytes da CGU / Objeto .o) │
└─────────────────────────┴────────────────────────────────────────────────┘
```

A chave primária de busca é estritamente o hash das entradas:
$$\text{CAS\_Key} = \text{BLAKE3}(\text{SourceBytes} \parallel \text{TargetLayout} \parallel \text{ToolchainDigest} \parallel \text{CompilerFlags} \parallel \text{DependenciesDigests})$$

### 4.3 Algoritmo de Evicção LRU (*Least Recently Used*)

O ciclo de vida do CAS é mantido por um monitor de baixo overhead acionado ao final de cada execução do CLI:

```text
Algoritmo: EnforceCacheBudget(max_budget_bytes)
1. current_size = catalog.get_total_size()
2. Se current_size <= max_budget_bytes:
     Retornar (Nenhuma ação necessária)
3. target_size = max_budget_bytes * 0.80  // Libera até 80% do teto para evitar thrashing
4. entries = catalog.query_entries_sorted_by_atime_ascending()
5. Para cada entry em entries:
     a. Tentar obter flock exclusivo em locks/entry.hash
     b. Se lock obtido:
          unlink(cas_path(entry.hash))
          catalog.delete(entry.hash)
          current_size -= entry.size
          liberar lock
     c. Se current_size <= target_size:
          Encerrar laço
6. catalog.commit()
```

### 4.4 Política de Depuração: *Line-Tables-First & Split DWARF*

Para resolver o segundo maior ofensor de espaço em disco (onde 85% dos arquivos de objeto são tabelas DWARF), o gerador de código Cranelift adota:

1. **Perfil `debug` (Desenvolvimento Padrão)**:
   - Emite apenas seções `.debug_line` e `.eh_frame`/`.pdata`.
   - Permite que qualquer pânico, assert ou breakpoint em depurador identifique com precisão cirúrgica o arquivo fonte e o número de linha.
   - **Economia comprovada: 75% a 85% de redução de tamanho de arquivo**.
2. **Perfil `debug-full` (Depuração Aprofundada com Variáveis)**:
   - Emite metadados DWARF completos contendo tipos e escopos de variáveis.
   - Em plataformas ELF (Linux), utiliza **Split DWARF (`-gsplit-dwarf`)**: os dados DWARF volumosos são emitidos em arquivos `.dwo` separados, impedindo que o linker de sistema perca tempo copiando megabytes de símbolos para dentro do executável.

### 4.5 Publicação Zero-Copy via Reflinks (Copy-on-Write)

Quando o linker in-process ou externo produz o binário final no CAS e precisa materializá-lo em `target/<profile>/meu_app`:

```rust
// arandu_cli/src/publish.rs
pub fn publish_artifact_cow(src: &Path, dst: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        // Tenta cópia atômica de blocos via ioctl FICLONE (Btrfs, XFS)
        if let Ok(src_file) = std::fs::File::open(src) {
            if let Ok(dst_file) = std::fs::File::create(dst) {
                use std::os::unix::io::AsRawFd;
                let ret = unsafe { libc::ioctl(dst_file.as_raw_fd(), libc::FICLONE, src_file.as_raw_fd()) };
                if ret == 0 {
                    return Ok(()); // Zero bytes físicos gastos! Clone instantâneo.
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        // Tenta clonefile nativo do Apple APFS
        use std::ffi::CString;
        let c_src = CString::new(src.to_str().unwrap()).unwrap();
        let c_dst = CString::new(dst.to_str().unwrap()).unwrap();
        if unsafe { libc::clonefile(c_src.as_ptr(), c_dst.as_ptr(), libc::CLONE_NOFOLLOW) } == 0 {
            return Ok(());
        }
    }

    // Fallback: Hardlink atômico no mesmo volume, ou stream copy como último recurso
    if std::fs::hard_link(src, dst).is_ok() {
        return Ok(());
    }
    std::fs::copy(src, dst).map(|_| ())
}
```

* **Resultado:** No Linux com Btrfs/XFS e no macOS com APFS (que cobre mais de 90% das máquinas modernas de desenvolvimento), a publicação do binário em `target/` consome **0 bytes adicionais de SSD**.

---

## 5. Invariantes de Arquitetura e Desvantagens (Drawbacks & Invariants)

### 5.1 Invariantes Preservados

1. **Salsa Permanece Puro e Sem I/O**: O motor Salsa não conhece caminhos físicos do CAS nem gerencia o banco de dados de cache. Ele opera sobre hashes em memória. A resolução e escrita de envelopes no CAS pertencem exclusivamente à camada orquestradora do CLI (`arandu_cli`).
2. **Determinismo Bit a Bit**: A chave do CAS é puramente função matemática das entradas. Um binário resgatado do cache é garantidamente idêntico ao binário que seria gerado por uma compilação fria.

### 5.2 Desvantagens e Custos de Complexidade

* **Sincronização Concorrente no CAS Global**: Se múltiplos processos do Arandu compilarem simultaneamente (ex: duas janelas do terminal rodando `arandu build` em projetos diferentes), o acesso ao CAS global exige *file locks* atômicos para evitar corrupção durante a escrita de artefatos.

---

## 6. Racional e Alternativas (Rationale & Alternatives)

### Alternativa 1: Manter o Diretório `target/` Monolítico do Cargo
* **Por que foi rejeitada:** Conduz diretamente à falha do Rust de consumir 10 GB a 50 GB por desenvolvedor, tornando a linguagem pesada e hostil em laptops com pouco armazenamento e em ambientes de CI.

### Alternativa 2: Exigir Limpeza Manual pelo Desenvolvedor (`arandu clean`)
* **Por que foi rejeitada:** A experiência da comunidade Rust comprovou que desenvolvedores não rodam comandos de limpeza preventiva até que seus discos estejam completamente lotados e o sistema operacional comece a travar. A auto-limpeza com teto estrito (LRU) é a única solução definitiva.

---

## 7. Arte Prévia e Literatura Científica (Prior Art)

1. **Go Build Cache (`GOCACHE`)**:
   - Introduzido no Go 1.10, o cache global por conteúdo eliminou pastas de objetos intermediários locais e estabeleceu o padrão de excelência da indústria para tempos de compilação instantâneos sem inchaço de disco.
2. **Zig Build Cache (`.zig-cache` e `zig-out`)**:
   - A separação entre saída solicitada (`zig-out/bin`) e repositório de objetos indexados por SHA-256 serviu de referência para a divisão entre produto final e intermediários.
3. **Buck2 / Nix / Bazel**:
   - Pioneiros no uso de Content-Addressable Storage com publicação por links simbólicos e hardlinks/reflinks CoW.
4. **Ferramentas `cargo-sweep` e `cargo-cache`**:
   - A existência e popularidade dessas ferramentas na comunidade Rust comprova a necessidade de integrar a gestão de orçamento de disco nativamente no compilador.

---

## 8. Questões em Aberto (Unresolved Questions)

1. **Interface de Configuração de Teto por Workspace**: Avaliar se projetos gigantescos devem ter a opção de declarar um teto de cache local isolado através do manifesto `arandu.toml` (`[build.cache] max_size = "5G"`).

---

## 9. Possibilidades Futuras (Future Possibilities)

1. **Remote Cache Distribuído em Nuvem (Estilo `GOCACHEPROG` / Bazel Remote)**: Permitir que equipes de engenharia compartilhem o mesmo CAS via S3 ou HTTP, fazendo com que uma biblioteca compilada por um engenheiro ou pelo CI esteja instantaneamente disponível para todos os outros desenvolvedores sem recompilar.
2. **Integração com Btrfs/ZFS/APFS Snapshots**: Criação de snapshots instantâneos do estado de compilação para depuração de regressões sem custo de espaço.
