#!/usr/bin/env python3
"""Render the bundled four-layer prompt. Missing fields fail rather than vanish."""
import argparse,json,re,sys
from pathlib import Path
from common import ROOT

def render(template:str,context:dict[str,str])->str:
    required=set(re.findall(r'\{\{([A-Z_0-9]+)\}\}',template));missing=required-context.keys()
    if missing:raise ValueError('missing prompt fields: '+','.join(sorted(missing)))
    if any(not isinstance(context[k],str) or not context[k].strip() for k in required):raise ValueError('empty/non-string prompt field')
    text=re.sub(r'\{\{([A-Z_0-9]+)\}\}',lambda m:context[m[1]],template)
    if re.search(r'\{\{[A-Z_0-9]+\}\}',text):raise ValueError('unresolved nested prompt field')
    return text

def main():
    p=argparse.ArgumentParser();p.add_argument('--role',choices=['commander','cto','developer','qa','reviewer','researcher'],required=True);p.add_argument('--context',type=Path,required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
    try:
        text=render((ROOT/f'resources/prompts/{a.role}.md').read_text(),json.loads(a.context.read_text()))
        a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(text,encoding='utf-8')
    except (ValueError,OSError) as e:print(e,file=sys.stderr);return 1
    return 0
if __name__=='__main__':sys.exit(main())
