#!/usr/bin/env python3
"""One native CLI invocation for bootstrapping HoH. No retries or supervisor loop.

Audit only: a native turn completing never means the product passed acceptance.
"""
from __future__ import annotations
import argparse, fcntl, json, os, re, shutil, subprocess, sys, uuid
from pathlib import Path
from common import ROOT, Terminated, atomic_json, execute, flag_supported, install_termination_handlers, load_json, restore_handlers, running_as_root, sha256, utcnow
from install_pi_plugins import verify_installed

# Set in every native child. A nested bridge call with --top-level is refused,
# so a delegated or already-running top-level session cannot start another commander.
PARENT_ENV='HOH_BOOTSTRAP_PARENT'
TOP_LEVEL_ROLES={'commander','developer'}
REPORT_ROLES={'qa','reviewer'}
# Session identity, IPC and per-session state that a parent agent session injects into its children
# (observed for Claude Code 2.1.317, documented for Codex). A delegated agent must start as an
# independent session, so only these are removed. User feature flags and provider/auth configuration
# (ANTHROPIC_*, CLAUDE_CODE_USE_BEDROCK, CLAUDE_CODE_EXPERIMENTAL_*, ...) are kept.
PARENT_SESSION_VARS=frozenset({'AI_AGENT','CLAUDECODE','CLAUDE_PID','CLAUDE_EFFORT','CLAUDE_CODE_SESSION_ID','CLAUDE_CODE_CHILD_SESSION',
    'CLAUDE_CODE_MESSAGING_SOCKET','CLAUDE_CODE_MESSAGING_TOKEN','CLAUDE_CODE_SESSION_ATTENDED','CLAUDE_CODE_ENTRYPOINT',
    'CLAUDE_CODE_EXECPATH','CODEX_SANDBOX','CODEX_SANDBOX_NETWORK_DISABLED'})

def child_env(attempt_id:str|None=None)->dict[str,str]:
    env={k:v for k,v in os.environ.items() if k not in PARENT_SESSION_VARS}
    if attempt_id:env[PARENT_ENV]=attempt_id
    return env

REQUIRED={
 'codex':['--json','--dangerously-bypass-approvals-and-sandbox'],
 'claude':['--output-format','--verbose','--dangerously-skip-permissions'],
 'pi':['--mode','--session-dir'],
 'grok':['--prompt-file','--output-format','--always-approve'],
}
def help_text(agent:str,cwd:Path)->str:
    cmd=[agent,'exec','--help'] if agent=='codex' else [agent,'--help']
    r=subprocess.run(cmd,cwd=cwd,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=20,check=False,env=child_env())
    if r.returncode:raise RuntimeError(f'{agent} help failed ({r.returncode})')
    return r.stdout.decode('utf-8',errors='replace')

def command_for(agent:str,session_dir:Path,model:str|None,help_output:str|None=None)->list[str]:
    if help_output is not None:
        missing=[x for x in REQUIRED[agent] if not flag_supported(help_output,x)]
        if missing:raise ValueError('unsupported native CLI flags: '+','.join(missing))
    if agent=='codex':cmd=['codex','exec','--json','--dangerously-bypass-approvals-and-sandbox','-']
    elif agent=='claude':cmd=['claude','-p','--dangerously-skip-permissions','--output-format','stream-json','--verbose']
    # Grok's headless single turn reads its prompt from a file; the bridge sends it on stdin like the others.
    # streaming-messages-json is Claude-style: system/init with session_id, messages, a final result.
    elif agent=='grok':cmd=['grok','--prompt-file','/dev/stdin','--output-format','streaming-messages-json','--always-approve']
    else:
        cmd=['pi','--mode','json','--session-dir',str(session_dir)]
        if help_output and flag_supported(help_output,'--approve'):cmd+=['--approve']
    if model:
        # Keep stdin marker last for Codex; never shell-interpolate the model name.
        if agent=='codex':cmd[-1:-1]=['--model',model]
        else:cmd+=['--model',model]
    return cmd

REPORT_FENCE=re.compile(r'```hoh-report[ \t]*\n(.*?)\n```',re.S)
FINAL_TEXT_LIMIT=1024*1024

def inspect_output(agent:str,path:Path)->dict:
    """Summarize a native JSONL stream. Failure is judged on terminal outcomes only: a transient error the
    native CLI retried itself (Codex `error` "Reconnecting...", Pi `stopReason:error` before auto-retry)
    is counted but does not fail a turn that later completes."""
    records=0;malformed=0;terminal=False;failed=False;transient=0;session_ids=set();assistant_text=[];models=set()
    final_text='';pi_last_stop=None;pi_retry_failed=False
    with path.open('rb') as f:
        while True:
            line=f.readline(1024*1024+1)
            if not line:break
            if len(line)>1024*1024:
                malformed+=1
                while line and not line.endswith(b'\n'):line=f.readline(1024*1024+1)
                continue
            if not line.strip():continue
            try:r=json.loads(line)
            except (ValueError,UnicodeError):malformed+=1;continue
            if not isinstance(r,dict):malformed+=1;continue
            records+=1;t=r.get('type','')
            text=None
            if agent=='codex' and t=='item.completed':
                item=r.get('item',{})
                if isinstance(item,dict) and item.get('type')=='agent_message':text=item.get('text')
            elif agent in ('claude','grok') and t=='result':text=r.get('result')
            elif agent=='pi' and t=='message_end':
                message=r.get('message',{})
                if isinstance(message,dict) and message.get('role')=='assistant':
                    content=message.get('content',[])
                    if isinstance(content,list):text=''.join(b.get('text','') for b in content if isinstance(b,dict) and b.get('type')=='text')
                    pi_last_stop=message.get('stopReason')
            if isinstance(text,str):
                assistant_text.append(text[-4096:]);assistant_text=assistant_text[-16:]
                if text.strip():final_text=text[-FINAL_TEXT_LIMIT:]
            # The terminal result alone carries the final answer: an earlier report must not outlive a last result without one.
            if agent in ('claude','grok') and t=='result':final_text=text[-FINAL_TEXT_LIMIT:] if isinstance(text,str) else ''
            for k in ['session_id','sessionId','thread_id']:
                if isinstance(r.get(k),str):session_ids.add(r[k])
            if agent=='pi' and t=='session' and isinstance(r.get('id'),str):session_ids.add(r['id'])
            if isinstance(r.get('model'),str):models.add(r['model'])
            if agent=='codex':
                terminal|=t=='turn.completed'
                failed|=t=='turn.failed'
                transient+=t=='error'
            elif agent in ('claude','grok') and t=='result':
                terminal=True;failed|=r.get('is_error',False) or r.get('subtype') not in [None,'success']
            elif agent=='pi':
                # JSON mode is a one-shot process: observed process exit is also
                # required by main. Production RPC MUST use negotiated settled.
                terminal|=t in ['agent_settled','agent_end']
                failed|=t in ['error','agent_error']
                if t=='auto_retry_start':transient+=1
                if t=='auto_retry_end':pi_retry_failed=r.get('success') is False
    if agent=='pi':failed|=pi_last_stop in ['error','aborted'] or pi_retry_failed
    return {'records':records,'malformed_records':malformed,'terminal_observed':terminal,'native_error_observed':failed,'transient_errors':transient,
            'session_ids':sorted(session_ids),'resolved_models':sorted(models),'assistant_text_tail':'\n'.join(assistant_text)[-8192:],'_final_text':final_text}

def build_report(role:str,final_text:str,session_ids:list[str],stdout:Path,attempt:Path)->dict:
    """Independent report from the agent's final `hoh-report` block. Identity and hashes are filled here,
    after the native process exited, so the agent can neither know nor forge them."""
    blocks=REPORT_FENCE.findall(final_text)
    if not blocks:raise ValueError('final assistant message has no ```hoh-report block')
    try:body=json.loads(blocks[-1])
    except ValueError as exc:raise ValueError(f'hoh-report block is not JSON: {exc}') from None
    if not isinstance(body,dict):raise ValueError('hoh-report block must be a JSON object')
    extra=set(body)-{'role','status','scope','findings'}
    if extra:raise ValueError('hoh-report block may only contain role, status, scope, findings; got '+', '.join(sorted(extra)))
    if body.get('role',role)!=role:raise ValueError(f'hoh-report role {body.get("role")!r} differs from --role {role}')
    if len(session_ids)!=1:raise ValueError(f'expected exactly one native session id, observed {len(session_ids)}')
    candidate=os.getenv('HOH_CANDIDATE_ID');spec=os.getenv('HOH_SPEC_SHA256')
    if not candidate or not spec:raise ValueError('--report-out needs HOH_CANDIDATE_ID and HOH_SPEC_SHA256 (run it under tools/run_check.py)')
    def ref(p:Path)->dict:return {'path':str(p.resolve().relative_to(ROOT.resolve())),'sha256':sha256(p)}
    report={'schema_version':'1.1','role':role,'session_id':session_ids[0],'candidate_id':candidate,'spec_sha256':spec,
            'status':body.get('status'),'scope':body.get('scope'),'findings':body.get('findings'),'raw_session':ref(stdout),'native_attempt':ref(attempt)}
    from jsonschema import Draft202012Validator
    errors=sorted(Draft202012Validator(load_json(ROOT/'contracts/independent-report.schema.json')).iter_errors(report),key=lambda e:str(list(e.path)))
    if errors:raise ValueError(f'independent report invalid at {list(errors[0].path)}: {errors[0].message}')
    return report

def cli_version(agent:str,cwd:Path)->str:
    try:
        r=subprocess.run([agent,'--version'],cwd=cwd,stdout=subprocess.PIPE,stderr=subprocess.STDOUT,timeout=20,check=False,env=child_env())
        text=r.stdout.decode('utf-8','replace').strip().splitlines()
        return text[0][:160] if r.returncode==0 and text else 'unknown'
    except (OSError,subprocess.TimeoutExpired):return 'unknown'

def live_session_agents()->list[str]:
    """Agents a live check's live-sessions.json may name (contracts/live-session.schema.json)."""
    return load_json(ROOT/'contracts/live-session.schema.json')['properties']['sessions']['items']['properties']['agent']['enum']

def record_live_sessions(agent:str,version:str,observed:dict)->None:
    """When run inside tools/run_check.py, append observed native sessions to the check's live-sessions.json."""
    directory=os.getenv('HOH_EVIDENCE_DIR');check=os.getenv('HOH_CHECK_ID');candidate=os.getenv('HOH_CANDIDATE_ID')
    if not (directory and check and candidate) or not observed['session_ids']:return
    path=Path(directory)/'live-sessions.json'
    with (Path(directory)/'.live-sessions.lock').open('a+') as lock:
        fcntl.flock(lock,fcntl.LOCK_EX)
        data=load_json(path) if path.exists() else {'schema_version':'1.0','check_id':check,'harness_candidate_id':candidate,'sessions':[]}
        model=observed['resolved_models'][0] if len(observed['resolved_models'])==1 else 'unknown'
        for sid in observed['session_ids']:data['sessions'].append({'agent':agent,'cli_version':version,'session_id':sid,'resolved_model':model})
        atomic_json(path,data)



def main(argv=None)->int:
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--agent',choices=list(REQUIRED),required=True)
    p.add_argument('--role',default='developer')
    p.add_argument('--prompt-file',type=Path,required=True)
    p.add_argument('--cwd',type=Path,default=ROOT)
    p.add_argument('--output-dir',type=Path)
    p.add_argument('--model')
    p.add_argument('--timeout',type=float,default=float(os.getenv('HOH_CHILD_TIMEOUT_SECONDS','3600')))
    p.add_argument('--dry-run',action='store_true')
    p.add_argument('--top-level',action='store_true',help='Only for the initial commander launched by start.sh; never use for delegated work.')
    p.add_argument('--report-out',type=Path,help='qa/reviewer only: after the session, write the independent report built from its final ```hoh-report block.')
    a=p.parse_args(argv)
    if a.timeout<=0 or not a.prompt_file.is_file() or not a.cwd.is_dir():p.error('positive timeout, existing prompt and cwd required')
    original=a.prompt_file.read_bytes()
    if not original.strip():p.error('prompt must not be empty')
    if a.top_level and a.role not in TOP_LEVEL_ROLES:p.error('--top-level requires role commander or developer')
    if a.report_out and a.role not in REPORT_ROLES:p.error('--report-out requires role qa or reviewer')
    if a.report_out and a.report_out.exists():p.error(f'report already exists: {a.report_out}; evidence is append-only')
    if os.getenv('HOH_EVIDENCE_DIR') and a.agent not in live_session_agents():
        # The release gate rejects a live session the schema does not accept, so the session would be evidence of nothing.
        print(f'--agent {a.agent} cannot run inside a check: contracts/live-session.schema.json accepts only '+', '.join(live_session_agents()),file=sys.stderr);return 64
    if a.top_level and os.getenv(PARENT_ENV):p.error(f'--top-level refused: already inside bootstrap session {os.getenv(PARENT_ENV)}; a nested session cannot start another top-level coordinator')
    if a.top_level:data=b'[HOH_TOP_LEVEL_BOOTSTRAP]\n'+original
    else:
        envelope=('[HOH_DELEGATED_TASK]\nRole: '+a.role+'\nYou are a scoped worker, NOT the top-level project commander. Execute only the task below. Do not start LAUNCH_PROMPT.md, bootstrap the whole project, advance the global taskboard, or recursively create another commander. Return a scoped result and stop. Project invariants still apply.\n\n')
        data=envelope.encode('utf-8')+original
    attempt_id=str(uuid.uuid4())
    # Inside run_check, attempts live in the check's evidence directory so the gate can prove provenance.
    base=Path(os.environ['HOH_EVIDENCE_DIR'])/'attempts' if os.getenv('HOH_EVIDENCE_DIR') else ROOT/'.hoh-dev'/'attempts'
    out=(a.output_dir or base/attempt_id).resolve()
    model=a.model or os.getenv('HOH_MODEL_'+a.agent.upper()) or None
    if a.dry_run:
        print(json.dumps({'dry_run':True,'capabilities_validated':False,'cwd':str(a.cwd.resolve()),'argv':command_for(a.agent,out/'sessions',model),'prompt_bytes':len(data)},ensure_ascii=False,indent=2));return 0
    if out.exists():print(f'Attempt directory already exists: {out}; attempts are append-only.',file=sys.stderr);return 64
    if not shutil.which(a.agent):print(f'Missing native CLI: {a.agent}',file=sys.stderr);return 127
    if running_as_root():
        print('Native agents run with bypass/YOLO flags; use the VM non-root development user, not root.',file=sys.stderr);return 78
    try:help_output=help_text(a.agent,a.cwd);cmd=command_for(a.agent,out/'sessions',model,help_output)
    except (RuntimeError,ValueError,subprocess.TimeoutExpired) as exc:print(str(exc),file=sys.stderr);return 78
    env=child_env(attempt_id);pi_packages=None
    if a.agent=='pi':
        # Pi installs missing configured packages at startup unless offline; load only verified installations (M0R3-05).
        pi_packages=verify_installed(a.cwd.resolve())
        if pi_packages['problems']:
            print('Refusing to start Pi with unverified project packages (run tools/install_pi_plugins.py): '+'; '.join(pi_packages['problems']),file=sys.stderr);return 78
        env['PI_OFFLINE']='1'
    if a.agent=='codex':
        git=subprocess.run(['git','rev-parse','--show-toplevel'],cwd=a.cwd,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
        if git.returncode:
            if '--skip-git-repo-check' not in help_output:print('Codex requires a Git workspace.',file=sys.stderr);return 78
            cmd.insert(-1,'--skip-git-repo-check')
    out.mkdir(parents=True,exist_ok=False)
    (out/'sessions').mkdir()
    (out/'prompt.md').write_bytes(data)
    (out/'native-help.txt').write_text(help_output,encoding='utf-8')
    version=cli_version(a.agent,a.cwd)
    record={'schema_version':'1.1','record_type':'bootstrap_native_attempt','attempt_id':attempt_id,'candidate_id':os.getenv('HOH_CANDIDATE_ID'),'check_id':os.getenv('HOH_CHECK_ID'),'parent_attempt':os.getenv(PARENT_ENV),'agent':a.agent,'cli_version':version,'role':a.role,'argv':cmd,'cwd':str(a.cwd.resolve()),'started_at':utcnow(),'prompt_sha256':sha256(out/'prompt.md'),'model_requested':model,'top_level_bootstrap':a.top_level,'env_removed':sorted(PARENT_SESSION_VARS&os.environ.keys()),'status':'RUNNING','product_accepted':False}
    if pi_packages is not None:record.update(env_set={'PI_OFFLINE':'1'},pi_packages=pi_packages)
    atomic_json(out/'attempt.json',record)
    print(f'Native {a.agent} invocation; logs: {out}',file=sys.stderr,flush=True)
    previous=install_termination_handlers()
    try:
        rc,timed,dur=execute(cmd,a.cwd.resolve(),out/'stdout.jsonl',out/'stderr.log',a.timeout,data,env=env)
        observed=inspect_output(a.agent,out/'stdout.jsonl');final_text=observed.pop('_final_text')
        ok=rc==0 and observed['terminal_observed'] and not observed['native_error_observed'] and observed['malformed_records']==0
        record.update(observed,exit_code=rc,timed_out=timed,duration_seconds=dur,finished_at=utcnow(),status='NATIVE_TURN_COMPLETED' if ok else 'NATIVE_FAILED',stdout_sha256=sha256(out/'stdout.jsonl'),stderr_sha256=sha256(out/'stderr.log'))
        atomic_json(out/'attempt.json',record)
        if ok:record_live_sessions(a.agent,version,observed)
        report_error=None
        if ok and a.report_out:
            try:atomic_json(a.report_out,build_report(a.role,final_text,observed['session_ids'],out/'stdout.jsonl',out/'attempt.json'))
            except (ValueError,OSError) as exc:
                report_error=str(exc);print('independent report not written: '+report_error,file=sys.stderr)
                record['report_error']=report_error;atomic_json(out/'attempt.json',record)
        print(json.dumps({'attempt_record':str(out/'attempt.json'),'native_completed':ok,'report':str(a.report_out) if a.report_out and not report_error and ok else None,'product_accepted':False},ensure_ascii=False))
        if report_error:return 65
        return 0 if ok else (rc if 1<=rc<=125 else 1)
    except (KeyboardInterrupt,Terminated) as exc:
        record.update(status='INTERRUPTED',finished_at=utcnow(),error=type(exc).__name__);atomic_json(out/'attempt.json',record);return 130
    except Exception as exc:
        record.update(status='BRIDGE_FAILED',finished_at=utcnow(),error=f'{type(exc).__name__}: {exc}');atomic_json(out/'attempt.json',record)
        print(str(exc),file=sys.stderr);return 1
    finally:restore_handlers(previous)
if __name__=='__main__':sys.exit(main())
