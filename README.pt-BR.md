<h1 align="center">Claudinio Brain</h1>

<p align="center">
  <strong>Memória bitemporal em grafo de conhecimento para agentes de IA.</strong><br>
  Um binário, um arquivo, sem servidor e sem modelo no caminho de escrita.
</p>

<p align="center">
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
  <a href="https://github.com/claudin-io/claudinio-brain/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/claudin-io/claudinio-brain/actions/workflows/ci.yml/badge.svg"></a>
  <img alt="Rust" src="https://img.shields.io/badge/rust-1.88%2B-orange.svg">
  <img alt="Platforms" src="https://img.shields.io/badge/platform-macOS%20%7C%20Linux-lightgrey">
</p>

<p align="center">
  <a href="README.md">English</a> ·
  <a href="README.pt-BR.md">Português</a>
</p>

---

Dê um banco vetorial a um agente e pergunte quanto custa um produto. Ele vai
encontrar todos os preços que você já escreveu e escolher um. Não tem como saber
qual ainda é verdade, porque "o preço é 20" e "o preço *era* 20" são a mesma
frase para uma medida de similaridade.

O `brain` guarda fatos numa linha do tempo, não numa pilha. Escrever um valor
novo não sobrescreve o antigo — **fecha** ele. Um registro só responde as duas
perguntas:

```console
$ brain remember --subject produto_a --predicate preco --value 20 --at 2026-01-01
created: produto_a preco 20

$ brain remember --subject produto_a --predicate preco --value 25 --at 2026-06-01
superseded: produto_a preco 25

$ brain get produto_a preco
produto_a preco 25

$ brain get produto_a preco --as-of 2026-03-01
produto_a preco 20

$ brain history produto_a preco
[2026-01-01T00:00:00Z] produto_a preco 20  (until 2026-06-01T00:00:00Z)
[2026-06-01T00:00:00Z] produto_a preco 25  (current)
```

Nada foi apagado, e nada precisou ser reembeddado para isso funcionar.

## Por que bitemporal

Duas linhas do tempo, rastreadas separadamente:

- **tempo de validade** — quando o fato foi verdade no mundo.
- **tempo de transação** — quando o brain ficou sabendo.

Manter as duas é o que permite distinguir três coisas que um único timestamp
colapsa em uma — e elas significam coisas muito diferentes para um agente:

| resultado | significado |
|---|---|
| **superseded** | mudou. O valor antigo *era* verdade, e deixou de ser. |
| **corrected** | estávamos errados. O valor antigo nunca foi verdade, então é retratado. |
| **reasserted** | falaram a mesma coisa de novo. Reforça, não duplica. |

Uma retratação deliberadamente **não** é o inverso de uma supersessão: ela não
reabre o que o fato retratado tinha fechado. "Isso estava errado" deixa aquele
período genuinamente desconhecido, e inventar uma resposta seria pior do que
admitir a lacuna.

## Relações são fatos

`link A rel B` é um fato cujo objeto é uma entidade, então o grafo herda
bitemporalidade de graça — um fornecedor que mudou é um intervalo fechado, não
uma linha deletada. Isso também significa que a resposta pode morar num lugar
que as palavras da pergunta nunca alcançam:

```console
$ brain link produto_a fornecido_por acme
$ brain remember --subject acme --predicate pais --value "Chile"

$ brain recall "de que pais vem o produto_a"
acme pais Chile
produto_a preco 25
produto_a fornecido_por acme
```

"Chile" não compartilha nenhuma palavra com a pergunta. Está um salto além da
entidade que a pergunta nomeia, e a relação é o mapa até lá.

## Como o recall funciona

Quatro canais independentes buscam candidatos, e *reciprocal rank fusion*
combina os resultados. Fundir em vez de escolher um é o ponto: concordância
entre sinais independentes é, por si só, evidência.

| canal | encontra |
|---|---|
| **bm25** | palavras, via FTS5. Insensível a acento, então "preco" acha "preço". |
| **alias** | entidades que a pergunta nomeia diretamente, pela chave ou por outro nome. |
| **semantic** | paráfrases, via embeddings estáticos compilados no binário. |
| **graph** | fatos alcançados caminhando pelas relações a partir do que foi nomeado. |

Tudo é filtrado temporalmente *antes* do ranqueamento, então o recall responde
com o que é verdade, não com tudo que já foi registrado. Um fato retratado não
aparece nem em `--as-of` nem em `--history`: ele nunca foi verdade, então
repeti-lo seria mentir.

O canal semântico é uma tabela de **embeddings estáticos** — uma consulta
token-para-vetor, não um transformer. Sem runtime ONNX, sem download, sem
toolchain C++ e sem amostragem, que é o que torna o recall reproduzível o
bastante para as baselines de eval existirem.

## Nomes

Uma entidade é guardada sob uma chave, mas as pessoas perguntam com outras
palavras.

```console
$ brain remember --subject acme --predicate funcionarios --value 40
$ brain remember --subject "Produto Brasília" --predicate preco --value 20
$ brain remember --subject servidor --predicate porta --value 8080

$ brain alias acme "ACME Corp"           # um nome que você declara
"acme_corp" now names acme

$ brain recall "quanto custa o produto brasilia" --learn
Produto Brasília preco 20
acme funcionarios 40
servidor porta 8080
(learned: "produto_brasilia" names Produto Brasília)

$ brain entity "Produto Brasília"
Produto Brasília (produto_brasília)
  also: produto_brasilia (learned)
  Produto Brasília preco 20
```

A pergunta perdeu o acento, então não nomeou nada — identidade é exata, e
`produto_brasilia` não é `produto_brasília`. O BM25 respondeu mesmo assim,
porque a busca é a camada tolerante, e o nome que funcionou foi guardado.

Os dois têm níveis de confiança bem diferentes, e essa separação sustenta o
resto:

- Um alias **declarado** decide identidade. Fatos futuros sobre "ACME Corp" caem
  em `acme`.
- Um alias **aprendido** só amplia a busca. Um palpite que pudesse decidir
  identidade deixaria uma única pergunta bem formulada enxertar todo o histórico
  futuro de uma entidade no nó errado, sem nada em nenhuma saída denunciando isso.

O aprendizado fica desligado a menos que você peça (`--learn`), porque uma
leitura que escreve é uma leitura que não pode ser repetida. `brain entity <nome>`
mostra todos os nomes de uma coisa e de que tipo cada um é.

## Isolamento

Um brain é exatamente um arquivo SQLite, e duas propriedades são impostas em vez
de prometidas:

- **`ATTACH` é impossível.** `SQLITE_LIMIT_ATTACHED` é zero em toda conexão,
  então nenhuma query alcança um segundo arquivo. Sem isso, uma instrução
  forjada poderia juntar os fatos de outro cliente no mesmo resultado.
- **Um arquivo só é um brain se ele disser que é.** `open` exige o marcador
  `brain_id`, então um `.db` qualquer nunca é adotado. Arquivos nascem `0600`.

Toda resposta JSON carrega `brain_id`, `brain_label` e `brain_path` — o último
porque copiar um arquivo de brain duplica o id, então identidade sozinha não
distingue duas cópias.

`brain where` explica qual brain seria usado, e por quê. A escada de resolução
tem oito degraus e nenhum fallback silencioso: um diretório sem brain é um erro,
nunca o global.

## Instalação

Precisa de um compilador C (para `onig`, SQLite embutido e `sqlite-vec`). Um
compilador C++ deliberadamente **não** é necessário — veja
[docs/stack-notes.md](docs/stack-notes.md).

```bash
cargo install --git https://github.com/claudin-io/claudinio-brain
```

Ou a partir de um clone:

```bash
git clone https://github.com/claudin-io/claudinio-brain.git
cd claudinio-brain
cargo build --release        # target/release/brain
```

O binário de release é um arquivo único e autocontido de cerca de 14 MB —
SQLite, o índice vetorial e 7,3 MB de pesos quantizados estão todos compilados
dentro. Nada é baixado em tempo de execução, e o `brain` nunca faz requisição de
rede: `model2vec-rs` é compilado com `local-only`, o que remove todo caminho de
rede em tempo de compilação.

## Comandos

```
init       Cria um brain novo
where      Mostra qual brain seria usado aqui, e por quê
stats      Reporta a identidade e o conteúdo do brain
remember   Registra um fato
link       Registra uma relação entre duas entidades
get        Lê o valor atual, ou o valor num instante passado
recall     Busca no brain com uma pergunta em linguagem natural
history    Mostra a trajetória completa de um par sujeito/predicado
entity     Mostra o que se sabe de uma entidade, e a que ela se conecta
why        Mostra de onde um fato veio e o que aconteceu com ele
retract    Marca um fato como nunca tendo sido verdade
alias      Dá outro nome a uma entidade, lista os nomes, ou remove um
reindex    Reconstrói o índice vetorial a partir dos embeddings guardados
predicate  Corrige a cardinalidade de um predicado
```

Todo comando aceita `--json`, e toda resposta JSON é carimbada com o brain que a
produziu. `--brain <caminho>`, `--use <nome>` e `--global` escolhem com qual
brain falar.

## Evals

Testes provam correção; evals medem qualidade. Um brain pode satisfazer todo
invariante bitemporal e ainda ser inútil se o recall nunca trouxer a resposta.

```bash
cargo run --example eval                      # mede e falha em regressão
cargo run --example eval -- --misses          # ...e nomeia os casos ainda errados
```

Quatro suítes — retrieval, temporal, graph, alias — cada uma pontuada contra
cada canal isolado e fundido, para que a contribuição marginal de um canal seja
um número e não uma opinião. `evals/baseline.json` é versionado e o CI falha em
regressão, o que significa que melhorar um número exige atualizar a baseline no
mesmo commit, onde ela entra no diff e é revisada.

[evals/README.md](evals/README.md) explica como ler a tabela de ablação, e tem
uma seção sobre o que essas suítes **não** conseguem medir.

## Status

Pré-1.0. O formato em disco ainda não é estável e não há caminho de migração
entre versões de schema.

Construído e funcionando: o store selado e a escada de resolução, o modelo
bitemporal de fatos, os quatro canais de recall, e nomes declarados e aprendidos.
Um servidor MCP é a próxima peça — a feature `mcp` do cargo está declarada e
ligada por padrão, mas nada a implementa ainda, então hoje o CLI e a biblioteca
Rust são as duas superfícies reais.

## Contribuindo

[CONTRIBUTING.md](CONTRIBUTING.md) cobre o setup e o que é uma mudança
mergeável. Relatos de segurança seguem o [SECURITY.md](SECURITY.md) — por favor
não abra uma issue pública para isso.

## Licença

MIT. Veja [LICENSE](LICENSE).
