"""ADR-032 S0 fixtures (design correction 4): the committed excerpts keep the structure of their recordings."""
import copy,importlib.util,json,shutil,tempfile,unittest
from pathlib import Path
from helpers import ROOT

FIXTURES=ROOT/'tests/fixtures/commander-readonly'

def load(name):
    spec=importlib.util.spec_from_file_location(name,FIXTURES/f'{name}.py')
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module);return module

excerpt_check=load('excerpt_check');excerpt=excerpt_check.excerpt

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

if __name__=='__main__':unittest.main()
