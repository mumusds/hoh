"""ADR-032 S0 fixtures (design correction 4): the committed excerpts keep the structure of their recordings."""
import copy,importlib.util,json,shutil,tempfile,unittest
from pathlib import Path
from helpers import ROOT

FIXTURES=ROOT/'tests/fixtures/commander-readonly'

def load(name):
    spec=importlib.util.spec_from_file_location(name,FIXTURES/f'{name}.py')
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module

excerpt_check=load('excerpt_check');excerpt=excerpt_check.excerpt;judge_check=load('judge_check')

def excerpt_side(report):
    """The verdicts on the committed excerpts (the raw recordings are only on the machine that recorded them)."""
    return {part:{k:v['excerpt'] for k,v in report[part].items()} for part in ('cases','rounds','generated','mutations')}

class ExcerptStructure(unittest.TestCase):
    def test_the_committed_excerpts_keep_their_recorded_skeletons(self):
        problems,stats=excerpt_check.check(FIXTURES/'excerpts',ROOT,recorded_only=True)
        self.assertEqual(problems,[]);self.assertGreaterEqual(stats['files'],24);self.assertGreater(stats['lines'],400)

    def test_a_flattened_field_is_caught(self):
        # The round-5 defect (M2T20D-400): passthrough.turn_id lifted into payload.turn_id.
        with tempfile.TemporaryDirectory() as d:
            out=Path(d)/'excerpts';shutil.copytree(FIXTURES/'excerpts',out)
            path=out/'codex/app-ro-new.rollout.excerpt.jsonl';lines=path.read_text().splitlines()
            n=next(i for i,l in enumerate(lines) if '"custom_tool_call"' in l)
            f=json.loads(lines[n]);p=f['payload'];p['turn_id']=p.pop('internal_chat_message_metadata_passthrough')['turn_id']
            lines[n]=json.dumps(f);path.write_text('\n'.join(lines)+'\n')
            problems,_=excerpt_check.check(out,Path(d),recorded_only=True)
            self.assertEqual(len(problems),1);self.assertIn('app-ro-new.rollout.excerpt.jsonl',problems[0])

    def test_an_excerpt_whose_structure_differs_from_its_raw_source_is_caught(self):
        with tempfile.TemporaryDirectory() as d:
            root=Path(d);raw=root/'raw.jsonl'
            raw.write_text(json.dumps({'payload':{'meta':{'turn_id':'t1'},'text':'x'}})+'\n'+json.dumps({'other':1})+'\n')
            text=excerpt.excerpt_file(raw,'raw.jsonl',lambda f:'payload' in f,[],'test')
            out=root/'excerpts/codex';out.mkdir(parents=True);(out/'case.stdout.excerpt.jsonl').write_text(text)
            self.assertEqual(excerpt_check.check(root/'excerpts',root,recorded_only=False)[0],[])
            # A hand-made excerpt with its own (consistent) skeleton hash but another structure than the source line.
            header,line=text.splitlines();f=json.loads(line);f['payload']['turn_id']=f['payload'].pop('meta')['turn_id']
            h=json.loads(header);h['_excerpt']['skeleton_sha256']=[excerpt.skeleton_sha256(f)]
            (out/'case.stdout.excerpt.jsonl').write_text(json.dumps(h)+'\n'+json.dumps(f)+'\n')
            problems=excerpt_check.check(root/'excerpts',root,recorded_only=False)[0]
            self.assertEqual(len(problems),1);self.assertIn('differs from source line 0',problems[0])
            self.assertEqual(excerpt_check.check(root/'excerpts',root,recorded_only=True)[0],[])

    def test_redaction_keeps_keys_nesting_and_types(self):
        f={'account_id':'acct-1','usage':{'input_tokens':3},'plugins':[{'name':'x','path':'/home/u/p'}],'cwd':'/tmp/w',
           'id':'01a1e58c-7355-7d83-8c50-b8aaf317a6e4','rate_limits':{'primary':{'used':12}}}
        r=excerpt.redact(copy.deepcopy(f),[('/tmp/w','<W>')])
        self.assertEqual(excerpt.skeleton(r),excerpt.skeleton(f))
        self.assertEqual((r['account_id'],r['plugins'][0]['name'],r['cwd'],r['rate_limits']['primary']['used'],r['usage']['input_tokens']),
                         ('<redacted>','<redacted>','<W>',0,3))
        self.assertTrue(r['id'].startswith('<uuid-'))

class Judge(unittest.TestCase):
    """judge_check.py, the evidence table's rules executed (design correction 4), gives the committed verdicts."""
    @classmethod
    def setUpClass(cls):
        with tempfile.TemporaryDirectory() as d:cls.report=judge_check.run(FIXTURES/'excerpts',Path(d))
    def test_the_verdicts_on_the_excerpts_are_the_committed_ones(self):
        committed=json.loads((FIXTURES/'judge_check.out.json').read_text())
        self.assertEqual(excerpt_side(self.report),excerpt_side(committed))
        self.assertEqual((committed['mismatches'],committed['defects']),([],[]))
    def test_every_read_only_round_of_the_v5_argv_passes_and_the_extension_writes_fail(self):
        rounds={k:v['excerpt']['verdict'] for k,v in self.report['rounds'].items()}
        for r in ('claude/ro-new','claude/ro-resumed','codex/app-ro-new','codex/app-ro-resumed','pi/noext-new','pi/noext-resumed'):
            self.assertEqual(rounds[r],'PASS',r)
        self.assertEqual((rounds['pi/ro-new'],rounds['pi/ro-resumed']),('FAIL','FAIL'))
    def test_no_generated_variant_of_a_positive_round_passes_and_the_invariants_hold(self):
        # Design correction 5: every mutant non-PASS, every invariance variant keeps PASS (M2T20D-500..502).
        self.assertEqual(self.report['defects'],[])
        self.assertEqual(sorted(self.report['generated']),sorted(judge_check.POSITIVE))
        for rnd,v in self.report['generated'].items():
            rows=v['excerpt'];self.assertGreater(len(rows),10,rnd)
            for r in rows:
                if r['expect']=='non-PASS':self.assertNotEqual(r['verdict'],'PASS',(rnd,r['kind'],r['label']))
                else:self.assertEqual(r['verdict'],'PASS',(rnd,r['kind'],r['label']))
    def test_the_round_6_probes_are_not_passes(self):
        # No control, a failed or interrupted turn, an earlier successful driver run before the refusal.
        for name in ('app-ro-new','app-ro-resumed'):
            c=judge_check.load(FIXTURES/'excerpts',Path('/nonexistent'),'codex',name)[0]
            control=judge_check.load(FIXTURES/'excerpts',Path('/nonexistent'),'codex','app-control')[0]
            self.assertEqual(judge_check.round_verdict(c,control)[0],'PASS')
            self.assertEqual(judge_check.round_verdict(c,None)[0],'INSUFFICIENT')
            for status in ('failed','interrupted'):
                m=copy.deepcopy(c)
                for f in m.stdout:
                    if f.get('method')=='turn/completed':f['params']['turn']['status']=status
                self.assertEqual(judge_check.round_verdict(m,control)[0],'INSUFFICIENT')
            m=copy.deepcopy(c);judge_check._insert_shell_success(m,'worktree','create',after=False)
            self.assertEqual(judge_check.round_verdict(m,control)[0],'FAIL')
    def test_the_round_7_probes(self):
        # Design correction 6: the round's own records only, each driver call complete, the adapters' settle rules.
        none=Path('/nonexistent')
        for name in ('app-ro-new','app-ro-resumed'):
            c=judge_check.load(FIXTURES/'excerpts',none,'codex',name)[0];control=judge_check.load(FIXTURES/'excerpts',none,'codex','app-control')[0]
            m=copy.deepcopy(c)
            for f in judge_check._driver_frames(m,started=True):judge_check._foreign_frame(f)
            self.assertEqual(judge_check.round_verdict(m,control)[0],'INSUFFICIENT')  # the only refusals belong to another turn
            m=copy.deepcopy(c);m.rollout.insert(0,{'type':'response_item','payload':{'type':'custom_tool_call_output','call_id':'old',
                'internal_chat_message_metadata_passthrough':{'turn_id':'old-turn'},'output':[{'type':'input_text',
                'text':json.dumps({'output':f"HOH-CELL worktree create {c.paths['P']}/hoh-shell-create exit=0 err=\n"})}]}})
            self.assertEqual(judge_check.round_verdict(m,control)[0],'PASS')  # a success of an earlier turn is not counted
            for kind in ('killed','empty output','last cell missing'):
                m=copy.deepcopy(c);judge_check._incomplete_driver(m,kind,alone=False)
                self.assertEqual(judge_check.round_verdict(m,control)[0],'INSUFFICIENT',kind)
        for agent,name in (('claude','ro-new'),('codex','app-ro-new'),('pi','noext-new')):
            c=judge_check.load(FIXTURES/'excerpts',none,agent,name)[0];control=judge_check.load(FIXTURES/'excerpts',none,agent,judge_check.CONTROL[agent])[0]
            for how in ('dropped','failed','interrupted'):
                mc=copy.deepcopy(control);judge_check._set_terminal(mc,how)
                self.assertEqual(judge_check.round_verdict(c,mc)[0],'INSUFFICIENT',(agent,how))
        c=judge_check.load(FIXTURES/'excerpts',none,'pi','noext-new')[0];control=judge_check.load(FIXTURES/'excerpts',none,'pi','control')[0]
        m=copy.deepcopy(c);judge_check._set_terminal(m,'provider error')
        self.assertEqual(judge_check.round_verdict(m,control)[0],'INSUFFICIENT')  # pi.rs: a provider error not cleared fails
        m=copy.deepcopy(c);judge_check._retried_error(m)
        self.assertEqual(judge_check.round_verdict(m,control)[0],'PASS')  # cleared by the later successful message
    def test_the_round_8_probes(self):
        # Design correction 7: the adapters' semantics (response correlation, claude's every frame, pi's read to exit),
        # and the rollout enumerated default-deny.
        none=Path('/nonexistent')
        c=judge_check.load(FIXTURES/'excerpts',none,'codex','app-ro-new')[0];control=judge_check.load(FIXTURES/'excerpts',none,'codex','app-control')[0]
        for field in ('thread','turn'):
            for mode in ('an id never sent','no id','an error'):
                m=copy.deepcopy(c);judge_check._bad_response(m,field,mode)
                self.assertEqual(judge_check.round_verdict(m,control)[0],'INSUFFICIENT',(field,mode))
        for syntax in ('a space before the parenthesis','two patches in one exec'):
            for outcome in (None,'unknown','success'):
                m=copy.deepcopy(c);judge_check._odd_patch_call(m,syntax,outcome)
                # Design correction 8: whitespace only separates, so a space before the parenthesis is the same patch
                # call, and its success is a write (FAIL); two patches with one item stay unknown (INSUFFICIENT).
                want='FAIL' if (syntax,outcome)==('a space before the parenthesis','success') else 'INSUFFICIENT'
                self.assertEqual(judge_check.round_verdict(m,control)[0],want,(syntax,outcome))
        m=copy.deepcopy(c);judge_check._odd_driver_command(m)
        self.assertEqual(judge_check.round_verdict(m,control)[0],'INSUFFICIENT')
        m=copy.deepcopy(c);judge_check._driver_before_handshake(m)
        self.assertEqual(judge_check.round_verdict(m,control)[0],'INSUFFICIENT')
        c=judge_check.load(FIXTURES/'excerpts',none,'claude','ro-new')[0];control=judge_check.load(FIXTURES/'excerpts',none,'claude','control')[0]
        m=copy.deepcopy(c);m.stdout.append(dict(copy.deepcopy(next(f for f in m.stdout if f.get('type')=='result')),session_id='other',is_error=True))
        self.assertEqual(judge_check.round_verdict(m,control)[0],'INSUFFICIENT')  # claude.rs reads every result frame
        c=judge_check.load(FIXTURES/'excerpts',none,'pi','noext-new')[0];control=judge_check.load(FIXTURES/'excerpts',none,'pi','control')[0]
        m=copy.deepcopy(c);m.stdout.extend(judge_check._tool_success_pair(m))
        self.assertEqual(judge_check.round_verdict(m,control)[0],'FAIL')  # pi.rs reads to the process's exit
        m=copy.deepcopy(c);judge_check._late_recovery(m)
        self.assertEqual(judge_check.round_verdict(m,control)[0],'PASS')
    def test_the_round_9_probes(self):
        # The replay registers requests as the adapters do (no pre-fill from the request log) and reads the recorded
        # external observations (M2T20D-800, -801).
        none=Path('/nonexistent');ex=FIXTURES/'excerpts'
        for name in ('app-ro-new','app-ro-resumed'):
            c=judge_check.load(ex,none,'codex',name)[0];control=judge_check.load(ex,none,'codex','app-control')[0]
            m=copy.deepcopy(c);m.requests=[]
            self.assertEqual(judge_check.adapter_replay(m).state,'settled')  # the pending map is the adapter's own
            self.assertEqual(judge_check.round_verdict(m,control)[0],'INSUFFICIENT')  # but the log must be the adapter's
            for edit in (lambda m:m.stdout.remove(judge_check._initialize_response(m)),
                         lambda m:judge_check._initialize_response(m).update(error={'code':-32000,'message':'x'}),
                         lambda m:judge_check._initialize_response(m).update(error={'code':-32601,'message':'x'}),
                         judge_check._initialize_after_thread):
                m=copy.deepcopy(c);edit(m)
                self.assertNotEqual(judge_check.adapter_replay(m).state,'settled',name)
                self.assertEqual(judge_check.round_verdict(m,control)[0],'INSUFFICIENT',name)
            m=copy.deepcopy(c);judge_check._observe(m,exit={'returncode':1,'how':'exited'})
            self.assertEqual(judge_check.round_verdict(m,control)[0],'PASS')  # the App Server's exit does not matter
        c=judge_check.load(ex,none,'pi','noext-resumed')[0];control=judge_check.load(ex,none,'pi','control')[0]
        self.assertEqual(judge_check.round_verdict(c,control)[0],'PASS')
        self.assertEqual(c.observations['opened'],c.observations['noted'][0])  # the same file, recorded live
        for change in ({'opened':None},{'noted':[]}):
            m=copy.deepcopy(c);judge_check._observe(m,**change)
            self.assertEqual(judge_check.adapter_replay(m).failure,'SESSION_MISMATCH',change)
        m=copy.deepcopy(c);judge_check._opened_other_inode(m)
        self.assertEqual(judge_check.adapter_replay(m).failure,'SESSION_MISMATCH')
        m=copy.deepcopy(c);judge_check._observe(m,opened=judge_check.UNOBSERVED)
        self.assertEqual(judge_check.round_verdict(m,control)[0],'INSUFFICIENT')
        c=judge_check.load(ex,none,'pi','noext-new')[0]
        m=copy.deepcopy(c);judge_check._observe(m,opened=None)
        self.assertEqual(judge_check.round_verdict(m,control)[0],'PASS')  # a new session's file is not checked
        c=judge_check.load(ex,none,'claude','ro-new')[0];control=judge_check.load(ex,none,'claude','control')[0]
        for change,failure in (({'prompt_fed':False},'PROMPT_NOT_DELIVERED'),({'logs_complete':False},'RAW_LOG_INCOMPLETE'),
                               ({'exit':{'returncode':1,'how':'exited'}},'is_error or exit')):
            m=copy.deepcopy(c);judge_check._observe(m,**change)
            self.assertEqual(judge_check.adapter_replay(m).failure,failure)
            self.assertEqual(judge_check.round_verdict(m,control)[0],'INSUFFICIENT',change)
    def test_the_round_10_probes(self):
        # The launch is an observation the replay builds the product's messages from; the request log is compared
        # with them under the comparison contract; every required observation is typed (M2T20D-900, -901, -950).
        none=Path('/nonexistent');ex=FIXTURES/'excerpts';J=judge_check
        def verdict(m,control):return J.round_verdict(m,control)[0]
        for name in ('app-ro-new','app-ro-resumed'):
            c=J.load(ex,none,'codex',name)[0];control=J.load(ex,none,'codex','app-control')[0]
            self.assertEqual(verdict(c,control),'PASS')
            self.assertEqual([J._comparable(q) for q in J.adapter_replay(c).sent],[J._comparable(q) for q in c.requests])
            m=copy.deepcopy(c);m.requests=[]
            self.assertEqual(J.adapter_replay(m).state,'settled')  # the replay never reads the request log
            self.assertEqual(verdict(m,control),'INSUFFICIENT')
            for edit in (lambda m:J._edit_observation(m,('launch',),J.UNOBSERVED),
                         lambda m:J._edit_observation(m,('launch','resume'),J.UNOBSERVED),
                         lambda m:J._edit_observation(m,('launch','profile'),'readonly')):
                m=copy.deepcopy(c);edit(m)
                self.assertTrue(J.adapter_replay(m).failure.startswith('NOT_OBSERVED'))
                self.assertEqual(verdict(m,control),'INSUFFICIENT')
            for edit in (lambda m:J._thread_request(m)['params'].update(sandbox='danger-full-access',approvalPolicy='on-request'),
                         lambda m:m.requests.remove(J._logged(m,'initialized')),
                         lambda m:J._thread_request(m)['params'].update(threadId='another-thread'),
                         lambda m:J._edit_observation(m,('launch','profile'),'yolo')):
                m=copy.deepcopy(c);edit(m)
                self.assertEqual(verdict(m,control),'INSUFFICIENT')
            for edit in (lambda m:J._logged(m,'initialize')['params']['clientInfo'].update(version='9.9.9'),
                         lambda m:J._logged(m,'turn/start')['params']['input'][0].update(text='another prompt')):
                m=copy.deepcopy(c);edit(m)
                self.assertEqual(verdict(m,control),'PASS')  # the two fields the contract does not compare
            mc=copy.deepcopy(control);J._edit_observation(mc,('launch','profile'),'read_only')
            self.assertEqual(verdict(c,mc),'INSUFFICIENT')
        c=J.load(ex,none,'pi','noext-resumed')[0];control=J.load(ex,none,'pi','control')[0]
        m=copy.deepcopy(c);J._edit_observation(m,('launch','resume'),J.UNOBSERVED);J._observe(m,noted=J.UNOBSERVED,opened=J.UNOBSERVED)
        self.assertEqual(verdict(m,control),'INSUFFICIENT')  # Codex's round-10 probe: never defaulted to a new session
        for path,value in ((('launch','resume'),J.UNOBSERVED),(('launch','resume'),'true'),(('launch','session_id'),''),
                           (('launch','session_id'),J.UNOBSERVED)):
            m=copy.deepcopy(c);J._edit_observation(m,path,value)
            self.assertTrue(J.adapter_replay(m).failure.startswith('NOT_OBSERVED'),(path,value))
            mc=copy.deepcopy(control);J._edit_observation(mc,path,value)
            self.assertEqual(verdict(c,mc),'INSUFFICIENT',(path,value))
        m=copy.deepcopy(c);J._observe(m,prompt_sent=J.UNOBSERVED,get_state_session_file=J.UNOBSERVED)
        self.assertEqual(verdict(m,control),'PASS')  # recorder-only, named
    def test_the_round_11_probes(self):
        # Typed JSON: a boolean or a float never equals an integer id; an observation is type-checked before its enum
        # test; exit's fields are closed (M2T20D-1000, -1001).
        none=Path('/nonexistent');ex=FIXTURES/'excerpts';J=judge_check
        def verdict(m,control):return J.round_verdict(m,control)[0]
        for name in ('app-ro-new','app-ro-resumed'):
            c=J.load(ex,none,'codex',name)[0];control=J.load(ex,none,'codex','app-control')[0]
            for edit in (lambda m:J._logged(m,'initialize').update(id=False),
                         lambda m:J._thread_request(m).update(id=True),
                         lambda m:J._logged(m,'turn/start').update(id=2.0)):
                m=copy.deepcopy(c);edit(m)
                self.assertEqual(verdict(m,control),'INSUFFICIENT')
                mc=copy.deepcopy(control);edit(mc)
                self.assertEqual(verdict(c,mc),'INSUFFICIENT')
            for value in ([],{},False,['read_only'],{'profile':'read_only'}):
                m=copy.deepcopy(c);J._edit_observation(m,('launch','profile'),value)
                self.assertTrue(J.adapter_replay(m).failure.startswith('NOT_OBSERVED'),value)
                self.assertEqual(verdict(m,control),'INSUFFICIENT',value)
        for agent,name in (('claude','ro-new'),('codex','app-ro-new'),('pi','noext-resumed')):
            c=J.load(ex,none,agent,name)[0];control=J.load(ex,none,agent,J.CONTROL[agent])[0]
            m=copy.deepcopy(c);J._edit_observation(m,('exit','signal'),9)
            self.assertTrue(J.adapter_replay(m).failure.startswith('NOT_OBSERVED'),agent)
            self.assertEqual(verdict(m,control),'INSUFFICIENT',agent)
            mc=copy.deepcopy(control);J._edit_observation(mc,('exit','signal'),9)
            self.assertEqual(verdict(c,mc),'INSUFFICIENT',agent)
            self.assertTrue(J._json_equal({'a':[1,{'b':None}]},{'a':[1,{'b':None}]}))
            for a,b in ((0,False),(1,True),(2,2.0),('1',1),([],{}),(None,False),({'a':1},{'a':1,'b':None})):
                self.assertFalse(J._json_equal(a,b),(a,b))
    def test_every_one_value_change_of_a_refusal_is_not_a_refusal(self):
        for case,m in self.report['mutations'].items():
            self.assertEqual(m['excerpt']['as recorded'],'12 refused',case)
            for change,summary in m['excerpt'].items():
                if change.startswith('patches changed'):self.assertEqual(summary,'8 insufficient, 4 refused')
                elif change!='as recorded':self.assertEqual(summary,'12 insufficient',(case,change))


class CodeMode(unittest.TestCase):
    """Design correction 8: code mode's calls by the grammar, not the recorded text."""
    def test_the_grammar_takes_the_recorded_forms_and_nothing_else(self):
        ok={'text(await tools.exec_command({cmd:"sh /t/cells.sh"}));\n':[('exec','sh /t/cells.sh')],
            'const result = await tools.exec_command({cmd:"sh /t",max_output_tokens:6000}); text(result);\n':[('exec','sh /t')],
            'text(await tools.apply_patch("p"));\ntext(await tools.apply_patch("q"));\n':[('patch','p'),('patch','q')],
            judge_check.DISCOVERY:[('discovery',)]}
        for src,want in ok.items():self.assertEqual(judge_check.code_mode_script(src),want,src)
        for src in ('text(eval("1"));','text(ALL_TOOLS.filter(t => store(t)));','text(ALL_TOOLS.filter(t => /w/.test(t.name.constructor)));',
                    'text(await tools.exec_command({cmd:"x",yield_time_ms:1}));','// @exec: {}\ntext(await tools.apply_patch("p"));',
                    'const r = await tools.apply_patch("p");\n','text(ALL_TOOLS[0]);','text(ALL_TOOLS.filter(t => tools.x(t)));'):
            self.assertIsNone(judge_check.code_mode_script(src),src)
    def test_a_script_of_several_statements_is_attributed_strictly_by_order_and_count(self):
        # Design correction 8 item 3, form (iv): every mismatch of statements and items, and every statement outside the
        # grammar, is an unknown outcome (INSUFFICIENT); a patch that ran by its position is a write (FAIL). Never PASS.
        none=Path('/nonexistent');J=judge_check
        for name in ('app-ro-new','app-ro-resumed'):
            c=J.load(FIXTURES/'excerpts',none,'codex',name)[0];control=J.load(FIXTURES/'excerpts',none,'codex','app-control')[0]
            rows=[r for r in J.run_mutants(c,control) if r['kind']=='code mode mismatch']
            self.assertEqual(len(rows),14,name)
            for r in rows:
                want='FAIL' if r['label'] in ("a completed script with the refusal as a patch's item",
                                              "a failed script, the refused patch's item before the discovery's") else 'INSUFFICIENT'
                self.assertEqual(r['verdict'],want,(name,r['label']))
            # The same script with its items in order and in number is the invariance variant (the first refused).
            m=copy.deepcopy(c);J._code_mode_call(m,J.DISCOVERY+f'text(await tools.apply_patch({json.dumps(J.patch_text(m,"P","create"))}));\n',
                                                 ['Script failed\nWall time 0.0 seconds\nOutput:\n','[]',J.PATCH_REFUSAL])
            self.assertEqual(J.round_verdict(m,control)[0],'PASS',name)
    def test_lexis_outside_the_grammar_is_unclassifiable(self):
        # M2T20D-1100..1102: a comment opener or a line terminator other than "\n" anywhere, a regex body starting with
        # "*", flags outside gimsuy or repeated, a regex at the script's start and a lone surrogate are no script; the
        # generated "code mode lexis" variants are all INSUFFICIENT.
        J=judge_check
        for src in ('text(ALL_TOOLS.filter(t => /*/.test("*/") || t.name.includes(")); await tools.exec_command({cmd:\'touch /x\'}); //")));',
                    'text(ALL_TOOLS.filter(t => t.description.includes("https://x")));','text(ALL_TOOLS.filter(t => /w/.test(t.name)));\r\n',
                    'text(ALL_TOOLS.filter(t => t.name.includes("a\u2028b")));','text(ALL_TOOLS.filter(t => /w/gg.test(t.name)));',
                    'text(ALL_TOOLS.filter(t => /w/x.test(t.name)));','text(ALL_TOOLS.filter(t => /a\\\nb/.test(t.name)));',
                    '/w/.test("x");','text(await tools.exec_command({cmd:"\\ud800"}));'):
            self.assertIsNone(J.code_mode_script(src),src)
        self.assertEqual(J.code_mode_script('text(ALL_TOOLS.filter(t => /w/gi.test(t.name)));'),[('discovery',)])
        self.assertEqual(J.code_mode_script('text(await tools.exec_command({cmd:"\\ud83d\\ude00"}));'),[('exec','\U0001F600')])
        none=Path('/nonexistent')
        for name in ('app-ro-new','app-ro-resumed'):
            c=J.load(FIXTURES/'excerpts',none,'codex',name)[0];control=J.load(FIXTURES/'excerpts',none,'codex','app-control')[0]
            rows=[r for r in J.run_mutants(c,control) if r['kind']=='code mode lexis']
            self.assertEqual(len(rows),12,name)
            for r in rows:self.assertEqual(r['verdict'],'INSUFFICIENT',(name,r['label']))

if __name__=='__main__':unittest.main()
