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
    def test_every_one_value_change_of_a_refusal_is_not_a_refusal(self):
        for case,m in self.report['mutations'].items():
            self.assertEqual(m['excerpt']['as recorded'],'12 refused',case)
            for change,summary in m['excerpt'].items():
                if change.startswith('patches changed'):self.assertEqual(summary,'8 insufficient, 4 refused')
                elif change!='as recorded':self.assertEqual(summary,'12 insufficient',(case,change))

if __name__=='__main__':unittest.main()
