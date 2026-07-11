# aas ⇄ asx Parity Spec

The behavioral contract `aas` (Rust) must reproduce from `asx` (TypeScript, v0.3.0). Every
`file:line` points into `/Users/june/personal/asx/src/`. This is the port checklist; wire
contracts (endpoints/headers/JSON shapes) must **not** drift.

This document separates inherited parity from aas-only extensions. As of v0.1.5, the extensions
are deterministic account sorting, the typed `usage --json` integration contract, portable
credential export/import, and optional passphrase-encrypted vault bundles; see §J.

---

## §A. Command matrix (`cli.ts`)

`KNOWN_CMDS = {list,ls,load,login,switch,rename,remove,status,exec,e,sharing,help}` (`cli.ts:961`).
**Default-command shim** (`:962-965`): `asx <tok> …` where `<tok>` isn't a known cmd/flag but
resolves via `getAccountByName` → rewrite to `asx e <tok> …`.

| Command | Args / flags | Core behavior (see section) |
|---|---|---|
| `list [provider]` `-u`,`-d` | provider optional; else all `listKnownProviders()` | §C list render; hides empty providers unless named; `-u` = `ensureFresh`+`getUsage` per acct; `-d` dumps secret |
| `load [provider] [name]` | +share flags (rejected) | Snapshot live cred → **system** profile; auto-scan all providers if none; email-dedup; `setProfileType(system)` |
| `login [provider] [name]` | `--long-lived`, +share | `runLoginFlow`; on success `setProfileType(isolated|system)` + `setShare` |
| `switch <prov> <name>` (`s`) | both required | `adapter.switchTo(name)` |
| `status [provider]` | | print `getActive(p)` per provider |
| `rename <from> <to>` | | `renameSecret` + `renameAccount` |
| `remove [prov] <name>` (`rm`) | variadic | `removeAccount` + `deleteSecret` |
| `exec <name> [target] [args…]` (`e`) | `-b`,`-d`; cross: `-s/-i/--share/--isolate/--keep-context`,`--` | §G |
| `sharing <name>` | +share | show/set share; agent+non-system only |
| `refresh <prov> <name>` | `--no-login` | `adapter.refresh`; needsRelogin→`runLoginFlow` unless `--no-login` |
| `proxy <name> <frontend>` | | §H standalone proxy; prints inject env; runs to SIGINT |

Program: name `asx`, v`0.3.0`, commander (`cli.ts:147-150`). `spawnNative` adds `shell:true`
only on win32 (`:47-49`).

---

## §B. Provider registry, aliases, naming

**Registry** (`providers/index.ts:8-15`): `claude`+`claude-code`→claudeCodeAdapter, `codex`,
`zai`→keyAdapter('zai'), `grok`→keyAdapter('grok'), `cursor`.
- `listKnownProviders()` = `[claude,codex,zai,grok,cursor]` (minus `claude-code` alias) (`:24-26`).
- `KNOWN_TARGET_PROVIDERS=[claude,codex,grok,zai,xai,openai]` (`:29`).
- `normalizeProvider` (`:32-39`): `claude-code`→claude, `xai`→grok, `openai`→openai, else validated-or-undefined.

**Alias layers** (all must exist): registry (`claude-code`→claude); `normalizeProvider`; `getProviderShortName`
(`cli.ts:18-27`: claude/codex/grok/cursor/zai, fallback strip `-code`+split `-`); provider-home
`normalizeProviderKey` (contains `claude`→claude, `xai`→grok — `profile-home.ts:15-20`,
`shared-state.ts:58-63`); `agentSpec` key = `provider.includes('claude')?'claude':provider` (`cli.ts:44`).

**`deriveAccountName(email,provider)`** (`cli.ts:29-33`): `(email?email.split('@')[0]:'personal') + '.' + getProviderShortName(provider)` → e.g. `e-ed.codex`.

**`resolveProviderName(a,b)`** (`cli.ts:113-123`): both→`{provider:norm(a),name:b}`; none→`{}`;
only a → `getAccountByName(a)` hit → `{provider:acct.provider,name:a}`, else `{provider:norm(a)}`.
**No dotted `name.provider` parsing** — one-arg is account-lookup then provider. `norm(p)=normalizeProvider(p)||p.toLowerCase()`.

**Share flags** `withShareFlags` (`cli.ts:129-135`) adds `--isolated/--shared/--share <c>/--isolate <c>`.
`resolveShareFlags`→`resolveShareSelection` (`shared-state.ts:101-112`): >1 set → throw
`Use only one of --isolated / --shared / --share / --isolate.`; isolated→`{provided,value:[]}`;
shared→`{provided,value:undefined}`; share→parsed list; isolate→`base.filter(!exclude)`.
Store convention: `undefined`=share-all, `[]`=isolate-all, `[..]`=subset.

---

## §C. Data model & storage (`storage/account-store.ts`, `secure-store.ts`)

**`<config>/accounts.json`** (zod `:6-24`): `{version:1, accounts:[AccountRecord]}`.
`AccountRecord = {provider, name, label?, email?, addedAt:ISO, share?:string[], profileType?:'system'|'isolated', meta?}`.
Written pretty + `chmod 0600`.

Ops (⚠ mixed matching): `addAccount` (`:49-67`) exact-string provider match, **global name
uniqueness** across providers (throw), dedupe by (provider,name). `listAccounts` exact match.
`getAccount/removeAccount/setShare/setProfileType` use `canonicalProvider=lowercase` (`:78-80`).
`setShare(undefined)` deletes field. `setProfileType('system')` also deletes `share`.
`getAccountByName` throws on ambiguous (>1 provider) (`:140-146`). `renameAccount` cross-provider
conflict check + updates active markers (`:160-201`).

**Active marker** = separate **`<config>/.active.json`** = `{ [canonicalProvider]:name, updated:ISO }`
(`:108-125`).

**secure-store** (`secure-store.ts`): `isMacClaude = darwin && provider contains 'claude'`.
- `setSecret` (`:24-35`): mac-claude→`writeClaudeKeychainCredential(claudeProfileService, val)` + rm profile file; else write `getProfileCredentialPath` `0600` (mkdir home `0700`).
- `getSecret` (`:37-49`): mac-claude→keychain first (if non-empty), then file; else file or null.
- `deleteSecret` (`:51-56`): mac-claude delete keychain; always `rm -rf` profile home.
- `renameSecret` (`:58-81`): move keychain entry (read old→write new→delete old) + `renameSync(fromHome,toHome)`.

**Profile home** (`profile-home.ts`): `NATIVE_CRED_FILE={claude:'.credentials.json',codex:'auth.json',grok:'auth.json'}` else `credential`.
`safeProfileDirName(provider,name)="{normKey}-{name}".replace(/[^A-Za-z0-9_.-]/g,'_')` (`:29-31`).
`getProfileHome=<profilesDir>/<safe>`; `getProfileCredentialPath=<home>/<nativeCredFile>`.

**aas hardening:** keep the legacy path mapping for zero-migration compatibility, but reject
global-name or resolved-home collisions under the account-store transaction lock before writing a
credential. Store mutations use fail-closed parsing, last-valid backups, fsync, and atomic replace;
rename/delete are fallible transactions with rollback instead of asx's best-effort ordering.

---

## §D. Keychain & paths

**Keychain** (`utils/claude-keychain.ts`): `SERVICE_PREFIX='Claude Code-credentials'`.
`getClaudeKeychainService(dir?)` = no dir → `'Claude Code-credentials'`; else
`'Claude Code-credentials-'+ hex(sha256(dir))[:8]` (`:7-11`). `user = os.username||$USER||'user'`.
Exact argv (each token separate arg):
```
security find-generic-password   -s <service> -a <user> -w          # read (stdout, trim, empty→null)
security add-generic-password    -s <service> -a <user> -w <raw> -U # write (throws on fail)
security delete-generic-password -s <service> -a <user>             # delete (errors swallowed)
```
⚠ Two hash inputs: live reads hash `getClaudeConfigDir()`; per-account vault hashes
`getProfileHome(provider,name)` (`secure-store.ts:20-22`). Non-mac → `.credentials.json` file.

**Paths** (`utils/platform.ts`): `expandHome` handles `~/`,`~\`. `configBase` = win `%APPDATA%|~/AppData/Roaming`,
mac `~/Library/Application Support`, linux `$XDG_CONFIG_HOME|~/.config`. `<config>=<base>/asx`;
accounts=`<config>/accounts.json`; profiles=`<config>/profiles`. Homes honor env
`CLAUDE_CONFIG_DIR`/`CODEX_HOME`/`GROK_HOME` (expandHome) else `~/.claude|.codex|.grok`;
auth paths `<home>/.credentials.json` (claude) / `<home>/auth.json` (codex,grok).

**JWT** (`utils/jwt.ts`): `decodeJwtClaims` = split `.`, base64url-decode `parts[1]`, JSON.parse, no
verify, any error→null. Rust: `base64 URL_SAFE(_NO_PAD)` tolerant + serde_json.

**bar** (`utils/bar.ts`): `renderBar(remPct,width=20)` = `█*round(pct/100*w)+░*rest`, color
≥90 green/≥70 yellow/else red, wrapped `[..]`. `formatReset(when)` = `resets <Mon d, hh:mm> (<Hh Mm> left|now)`,
NaN→''. (In `aas`, meters carry raw pct + reset; CLI renders.)

---

## §E. Sharing / symlink (`storage/shared-state.ts`)

`SHARE_CATEGORIES=[sessions,skills,agents,hooks,settings]`. `SHARED` map per provider (`:16-52`),
each `{name,type:'dir'|'file',cat}`:
- **claude**: sessions=`projects/ sessions/ shell-snapshots/ file-history/ plans/ tasks/ todos/ history.jsonl`; skills=`skills/`; agents=`agents/`; hooks=`hooks/`; settings=`plugins/ settings.json CLAUDE.md`.
- **codex**: sessions=`sessions/ archived_sessions/ history.jsonl session_index.jsonl`; skills=`skills/`; settings=`rules/ plugins/ AGENTS.md config.toml`.
- **grok**: sessions=`sessions/ projects/ active_sessions.json`; skills=`skills/`; settings=`completions/ config.toml`.
- Never shared: native auth file, caches, logs, sqlite, tmp. `INJECTED_WHEN_CROSS={config.toml}` (skipped on cross).

`linkSharedState(provider,home,{isCross?,categories?})` (`:118-148`): base=`~/.claude|.codex|.grok`;
skip self-link; per entry: skip if category not allowed, skip if cross && config.toml; dir target
missing→mkdir, file target missing→skip; existing symlink→replace, existing real file→skip;
`symlinkSync(target→link)`. Direction: link in profile home → target in system home.
`supportedShareCategories`: claude all5, codex/grok = sessions/skills/settings. `describeShare`
(`:151-159`) → `shared: a,b (isolated: c)` style.

---

## §F. Provider adapters — wire contracts

### Claude (`providers/claude-code.ts`) — PROVIDER='claude'
- Consts: `CLIENT_ID='9d1c250a-e61b-44d9-88ed-5944d1962f5e'`, `LONG_LIVED_TOKEN_TYPE='claude-code-oauth-token'`.
- Cred: OAuth `{claudeAiOauth:{accessToken,refreshToken,subscriptionType,rateLimitTier,expiresAt(ms)}}` or long-lived `{type,token}`.
- Token normalize (`:20-42`): strip `export CLAUDE_CODE_OAUTH_TOKEN=` + quotes; `getClaudeCodeOAuthToken`=long-lived.token | claudeAiOauth.accessToken | accessToken | raw.
- Live read `readCurrentCredentials` (`:65-101`): mac+`CLAUDE_CONFIG_DIR`→scoped keychain then file; mac plain→services `[scoped,'Claude Code - credentials','claude-code-credentials']` then file; else file.
- Write `writeActiveCredentials` (`:114-127`): mac→keychain; else file `0600`.
- `loadCurrent` (`:182-204`): env `CLAUDE_CODE_OAUTH_TOKEN`→loadLongLivedToken; else read+extract email; scoped-email mismatch→throw; setSecret+addAccount.
- `switchTo` (`:206-213`): non-long-lived→writeActiveCredentials; setActive.
- `isExpired` (`:252-260`): `expiresAt < now+60000`.
- `refresh` (`:262-294`): `POST console.anthropic.com/v1/oauth/token` `{grant_type:'refresh_token',refresh_token,client_id}`; `invalid_grant`→needsRelogin; success→rebuild claudeAiOauth (`expiresAt=now+expires_in*1000`), setSecret, sync native if it held old.
- `getLoginCommand`=`['claude','auth','login']`. `loadLongLivedToken` (`:367-372`).
- **`getUsage`** (`:296-365`) via `fetchAnthropicJson(path,token)` (base `https://api.anthropic.com`, headers `Authorization:Bearer`, `anthropic-version:2023-06-01`, `anthropic-beta:oauth-2025-04-20`; curl fallback on throw; 15s):
  - baseInfo `subscription=<subType> tier=<tier>[ org=<orgType> has_max=<yes|no>] (name)` after `GET /api/oauth/profile` (org.organization_type||billing_type; acc.has_claude_max||org.has_claude_max).
  - `GET /api/oauth/usage`: 401/403→"token expired…Re-login"; 429→"rate limited[…retry after Ns]"; parse `five_hour|fiveHour`,`seven_day|sevenDay` `.utilization` → `5h:`/`7d:` bars; none→"no quota data". **On 401/403 no stale fallback.**

### Codex (`providers/codex.ts`) — P='codex', native `<CODEX_HOME>/auth.json`
- Cred: `{email?, tokens:{access_token,refresh_token,id_token,account_id}, account_id?}`.
- `extractCodexEmail`=email | jwt(id_token).email|email_address. `extractPlanFromIdToken`=claims['https://api.openai.com/auth'].{chatgpt_plan_type,chatgpt_subscription_active_until}. `codexReset`=reset_at*1000 | now+reset_after_seconds*1000.
- **`attemptCodexNativeRefresh`** (`:53-91`): the doctor trick — `execSync('codex doctor --summary',{env:{CODEX_HOME:getProfileHome('codex',name)},timeout:20000})` (fallback `codex login status` 8s), re-read auth.json, addAccount, sync shared if matched.
- `loadCurrent` (`:145-152`) throw if no auth.json. `isExpired` (`:160-168`): jwt(access_token||id_token).exp*1000<now+60000. `refresh`→attemptCodexNativeRefresh. `getLoginCommand`=`['codex','login']`.
- **`getUsage`** (`:178-241`): `GET https://chatgpt.com/backend-api/wham/usage` headers `Authorization:Bearer`,`Accept:application/json`,`User-Agent:codex-cli`,`ChatGPT-Account-Id:<accountId>`(if any). Parse `rate_limit|rate_limits`.{primary_window|primary, secondary_window|secondary}.used_percent → 5h/7d; planType=plan_type|extractPlan. Auth-fail (401/403 or regex)→attemptCodexNativeRefresh+retry once.

### Grok + Z.AI (`providers/key-adapter.ts` `createKeyAdapter`)
- `ZAI_BASE='https://api.z.ai/api/coding/paas/v4'`, `ZAI_QUOTA='https://api.z.ai/api/monitor/usage/quota/limit'`. `getEnvKey`=`<PFX>_API_KEY|<PFX>_KEY|(grok)XAI_API_KEY`.
- Grok auth file: obj w/ `key` (or map, first value); `grokBearer`=.key|firstval.key|raw; `writeGrokAuth` wraps as `{asx:{key}}` if raw. `parseGrokTokenInfo`= jwt only if starts `ey`.
- ZAI cred = bare key. `testZaiKey`=`GET <ZAI_BASE>/models` `Authorization:Bearer <key>`.
- `switchTo` (`:132-144`): grok→writeGrokAuth+`XAI_API_KEY`; else `<PFX>_API_KEY` env. `getLoginCommand`: grok `['grok','login']` else null. `login` (`:118-131`): zai only, prompt/`ASX_ZAI_API_KEY`, testZaiKey, setSecret+addAccount+setActive. **No isExpired/refresh.**
- **`getUsage`** (`:145-303`):
  - **Grok JWT** (key starts `ey`): `GET cli-chat-proxy.grok.com/v1/billing` (`config.monthlyLimit.val`,`config.used.val`→`credits: bar rem%/used% (used/limit)`, `billingPeriodEnd`) + `/v1/settings` (plan name).
  - **Grok apikey**: `GET api.x.ai/v1/api-key` (remaining_balance/spent_balance/total_granted→`credits: … ($rem left)`; key name). Rate limits: `GET api.x.ai/v1/models` headers `x-ratelimit-remaining-requests|-tokens` (or probe `POST /chat/completions {model:'grok-4.20-non-reasoning',…,max_tokens:1}`). Assemble `Grok <keyName>[ tier=..][ team=..]`.
  - **Z.AI**: `GET <ZAI_QUOTA>` headers **`Authorization:<raw key>` (NO Bearer)**, `Accept-Language:en-US,en`, `Content-Type:application/json`. Parse `data.limits|limits` find `type==='TOKENS_LIMIT'`.percentage (parsePercent: ≤1 & no `%`→*100) → `5h: bar`.

### Cursor (`providers/cursor.ts`) — metadata marker only
`loadCurrent`=setSecret `{note,name}`+addAccount; `switchTo`=setActive only; `getUsage`=static; no login/refresh.

---

## §G. exec flow (`cli.ts:735-957`, `exec-args.ts`)

1. `getAccountByName(name)`→profileProvider/accountName (`:736-758`).
2. Reparse argv for optional `<target>` (`:760-771`): first non-flag after name if `isKnownProvider` → `specifiedProvider=normalizeProvider(it)`, dropped from rawAfter.
3. `agentProvider=specifiedProvider||profileProvider`; `isCross = specified && norm(specified)≠norm(profile)`. `spec=agentSpec(agentProvider)` (else error).
4. **`parseExecArgs(rawAfter,{isCross,agentProvider})`** (`exec-args.ts:27-96`): `--`→rest to forwardArgs+break; `-b/--bypass`; `-d/--debug`; **cross only**: `-i/--isolated`,`-s/--shared`,`--share <v>|=v`,`--isolate <v>|=v`,`--keep-context`; else→forwardArgs. Returns `{forwardArgs,bypass,debug,keepContext,share:resolveShareSelection(...,isCross?agent:undef)}`.
5. `ensureFresh(profileProvider,accountName,debug)`; fail→exit1 with re-login hint (`:814`).
6. Claude long-lived: if `isClaudeCodeLongLivedToken(secret)`→token; non-cross claude→`env.CLAUDE_CODE_OAUTH_TOKEN=token`.
7. `systemProfile = profileType==='system' || isCurrentSystemProfile`.
8. **Same-provider** (`:829-860`): system→native home, no override/symlink; guard stored≠live→exit1 "asx switch". isolated→`env[spec.homeEnv]=getProfileHome`, `seedAgentHome`, `linkSharedState(...,{isCross:false,categories:acct.share})`; cred already at `<home>/<nativeFile>`.
9. **Cross** (`:861-923`): `home=crossSessionAgentHome(agent,name)` (uuid), `env[spec.homeEnv]=home`, seed, `linkSharedState(agent,home,{isCross:true,categories: share.provided?share.value:undefined})`. If `spec.stub` (claude) write `<home>/<file>` stub `0600`. Read backend secret; `startProxyForExec({sourceProvider:agent, targetProvider:profile, targetCredential:{apiKey,raw, type: profile==='claude'?'anthropic':'openai'}})`; `injectProxyEndpoint(agent,env,url,env[spec.homeEnv],profile)`.
10. `bypass`→prepend `getBypassFlags(agent)` to forwardArgs; `debug`→`ASX_DEBUG=1`. `spawnNative(bin,forwardArgs,{env,stdio:inherit})`. SIGINT→130/SIGTERM→143 after cleanup; exit→cleanup+exit(code). `cleanup`: stop proxy, `removeCrossSessionAgentHome` unless keepContext (`ASX_KEEP_CONTEXT=1` too).

**AGENT_SPEC** (`cli.ts:38-45`): codex{bin codex, CODEX_HOME, auth.json, bypass `--dangerously-bypass-approvals-and-sandbox --dangerously-bypass-hook-trust`, stub null}; claude{claude, CLAUDE_CONFIG_DIR, .credentials.json, `--dangerously-skip-permissions`, stub `{"claudeAiOauth":{"accessToken":"asx-proxy-dummy"}}`}; grok{grok, GROK_HOME, auth.json, `--dangerously-skip-permissions`, null}.
`seedAgentHome` (`cli.ts:58-64`): **claude only** — merge `<dir>/.claude.json` `{hasCompletedOnboarding:true}`.
`agentScratchHome`=`<profiles>/.agents/<safe>` (persistent, proxy cmd); `crossSessionAgentHome`=`<profiles>/.agents/sessions/<safe>-<uuid>` (ephemeral); `removeCrossSessionAgentHome` refuses paths outside `.agents/sessions/`.

---

## §H. Proxy (`proxy/`)

### Server (`server.ts`)
- `startProxy(opts)`→`{url,port,stop}`; bind `127.0.0.1:0` (Rust: keep the `TcpListener`, skip asx's free-port race `:308-318`). `agent=pickAgent(source)`, `backend=pickBackend(target)` once. `cred=targetCredential.raw||apiKey`.
- Routing (`:21-67`): `isInference=POST /(v1/)?(responses|messages|chat/completions|completions)/` (unanchored); `isModels=GET /(v1/)?models/?$`. (a) models→`agent.formatModels(backendChoices)` or default OpenAI list; (b) other→`200 {ok:true,authenticated:true,via:'asx-proxy'}` (fake-auth catch-all); (c) inference.
- Client-disconnect: `res.on('close')`→`clientClosed` if `!writableEnded`.
- Inference (`:57-153`): read body→JSON(fail→{}); `common=agent.parseRequest(path,body)`; `up=backend.buildRequest(common,cred)`; `fetchUpstreamWithRetry`; ctx=`{id:'chatcmpl-asx-'+reqId,created,model,first:true,toolNamespaces}`.
  - **error/`errText`≠null**: msg `[asx-proxy] backend <b> error <status>: <detail[:300]>`; stream→`formatStreamChunk(text)+done`; else `formatResponse({text})`. **HTTP 200 always.**
  - **stream happy**: `writeHead(200, streamHeaders)`; `forEachUpstreamEvent`: `tool_call_delta`→accumulate(hold); `done`→flush tools THEN write done; else write chunk. After loop flush; `!sawDone`→synthetic warn text+done (unless clientClosed); catch→flush+`stream interrupted`+done; finally `body.cancel()`+`res.end()`.
  - **non-stream**: accumulate text/tools/finish; `formatResponse`.
  - Outer catch: clientClosed→return; `!headersSent`→500 JSON; else `res.end()`.
- **`forEachUpstreamEvent`** (`:179-212`): frame on `\n\n` over accumulated buffer, `\r\n`→`\n`, streaming UTF-8 decode, flush trailing block, `isCancelled` breaks. One block→`backend.parseStreamChunk`.
- **`toolAccumulator`** (`:216-235`): merge `tool_call_delta` by `index` (id/name on open, argsDelta append), first-seen order.
- **`fetchUpstreamWithRetry`** (`:240-300`): retries=4 (5 total). RETRYABLE `{408,429,500,502,503,504}`; FATAL `{400,401,403,404,405,410,422}` never; network throw retry unless `/(auth|forbidden|invalid (url|api key)|cert|hostname)/i`. Happy stream: `res.ok && (ct event-stream || !backend.isRetryable)`→return `{res}` (no body read). Else read text; FATAL→`{res,errText}`; retryable = RETRYABLE.has || `backend.isRetryable(status,text)` → continue if attempt<retries. Backoff `min(30000,500*2^(n-1))+rand(0..499)`; per-attempt `AbortSignal.timeout(120000)`. `errText`⇒body consumed⇒failure even at 200.

### COMMON types (`types.ts`)
`CommonToolCall{id,name,arguments:string}`. `CommonMessage{role:system|user|assistant|tool,content,toolCalls?,toolCallId?,toolName?,isError?}`. `CommonToolDef{name,description?,parameters?,strict?,builtinType?}`. `CommonRequest{model,system?,messages,tools?,toolNamespaces?,toolChoice?,parallelToolCalls?,stream,maxTokens?,temperature?,reasoningEffort?}`. `CommonEvent = text|tool_call_delta{index,id?,name?,argsDelta?}|tool_call{id,name,arguments}|done{finishReason?}|error{message}`. `CommonResponse{text,toolCalls?,finishReason?}`. `StreamCtx{id,created,model,first,acc?,itemId?,textOpen?,textIndex?,nextIndex?,items?,toolNamespaces?}`.
`AgentAdapter{parseRequest,streamHeaders,formatStreamChunk,formatResponse,formatModels}`; `BackendAdapter{buildRequest,parseStreamChunk,isRetryable?}`.
Continuity = full-transcript replay (no server session store). Tool ids round-trip via id/toolCallId.

### Adapters (`adapters/`) — norm: contains 'claude'→claude
- **claude** frontend=Anthropic Messages SSE (`message_start`/`content_block_*`/`message_delta`/`message_stop`, `ZERO_USAGE`, model id `claude-asx-` wrap for `/^(claude|anthropic)/i` picker). backend=`POST api.anthropic.com/v1/messages?beta=true` headers `authorization:Bearer <token>`,`anthropic-version:2023-06-01`,`anthropic-beta:claude-code-20250219,oauth-2025-04-20`,`anthropic-dangerous-direct-browser-access:true`; system[0]=`"You are Claude Code, Anthropic's official CLI for Claude."`; **no temperature/top_p/top_k**; `thinking:{type:disabled}` unless model=Fable; token from claudeAiOauth.accessToken|type-token.
- **codex** backend=`POST chatgpt.com/backend-api/codex/responses` (Responses API) headers `Authorization:Bearer`,`chatgpt-account-id`,`OpenAI-Beta:responses=experimental`,`originator:codex_cli_rs`,`accept:text/event-stream`,`session_id:uuid`; body `{model,instructions,input,stream,store:false,tools,tool_choice,parallel_tool_calls,reasoning:{effort:choice.effort||req.reasoningEffort||'low'}}`. frontend=Responses SSE (`response.created/output_item.added/output_text.delta/...completed`); **namespace flatten** `ns__name`↔`{namespace,name}` (`parseTools`/`splitNamespaced`) for multi-agent; `codexModelInfo` full ModelInfo.
- **grok** frontend=OpenAI Chat chunks (`data: [DONE]`). backend=`POST cli-chat-proxy.grok.com/v1/chat/completions` headers `Authorization:Bearer <grokToken>`,`X-XAI-Token-Auth:xai-grok-cli`,`x-grok-client-version:0.2.77`,`x-grok-client-identifier:grok-shell`,`User-Agent:grok-shell/0.2.77 (macos; aarch64)`,`x-grok-model-override`. **drops `reasoning_content`**.
- **zai** backend only=`POST api.z.ai/api/coding/paas/v4/chat/completions` headers `Authorization:Bearer <cred>`,`Accept-Language:en-US,en`. **GLM thinking**: effort→`body.thinking={type: (none|off)?'disabled':'enabled'}` (NOT reasoning_effort). `isRetryable=isZaiOverload` codes `1301/1302/1304/1305` or overload regex (even 200).
- **util**: `sseData/sseEvent/sseHeaders`, chat↔common helpers, `parseChatToolDeltas`.

### Injection (`inject.ts`) & models (`models.ts`)
`injectProxyEndpoint(source,env,url,authToken,tmpDir?,backend?,bypass)`: codex→write `config.toml`(`model_provider="asx-proxy"`,`base_url="<url>/v1"`,`env_key="ASX_PROXY_API_KEY"`,`wire_api="responses"`,`requires_openai_auth=false`)+`models.json`, set the explicit scratch `CODEX_HOME` and `ASX_PROXY_API_KEY=authToken`; claude→`ANTHROPIC_BASE_URL=url`, replace inherited credentials with `ANTHROPIC_AUTH_TOKEN=authToken`, del `CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY`, slots OPUS/SONNET/HAIKU/FABLE→`ANTHROPIC_DEFAULT_<SLOT>_MODEL[_NAME|_DESCRIPTION]`, `ANTHROPIC_MODEL`,`OPENAI_BASE_URL=url`; grok→an explicit scratch `GROK_HOME` with `config.toml` `[models]default`+per-model `[model."<id>"]`(`base_url="<url>/v1"`,`api_backend="chat_completions"`,`api_key=authToken`,`context_window=200000`). `[ui]permission_mode="always-approve"` is present only when `bypass=true`. All config writes are atomic and `0600`; every proxy route validates the per-run token.
`models.ts`: `BackendChoice{id,model,effort?}`. `defaults`: codex `gpt-5.5-{high,medium,low,xhigh}`; claude `[opus-4-8,sonnet-4-6,haiku-4-5-20251001]`; grok `grok-build`; zai `[glm-5.2/high, glm-5.2-max/max, glm-5.2[1m]/high, glm-4.5-air]`. Precedence `env ASX_<PROV>_MODELS > <config>/models.json > defaults`. `resolveChoice(provider,id)`=find by id||list[0].

---

## §I. Fixes already in asx (carry into aas)
1. `list` hides empty providers (only `(none)` when a provider is explicitly named).
2. Codex `getLoginCommand=['codex','login']` (not bare `codex`).

## §J. New in aas
- `import` without a file inspects/adopts the shared asx paths. `export --all` and
  `import <file|->` move a versioned portable bundle containing account metadata and aas-managed
  provider credentials.
- `export --all --vault` encrypts the portable JSON using the standard age passphrase format
  (scrypt recipient and authenticated encryption). Import detects the age header automatically;
  passphrases come from a no-echo terminal prompt or short-lived `AAS_VAULT_PASSPHRASE`.
- Structured `Usage{headline,plan,meters[],notes,error}` returned by adapters; CLI renders
  table/bars and `usage --json` exposes the typed integration contract used by aas-bar/BarShelf.
- Parallel `list -u` / `usage` (fan-out fetch, ordered single render).
- Deterministic display ordering: fixed provider registry order, then case-insensitive account
  name by default. `--sort added` uses `addedAt`; `--sort stored` preserves the `accounts.json`
  array order. Sorting is a view operation and never rewrites the store.
