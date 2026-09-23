import sys
import unittest
from pathlib import Path
sys.path.insert(0,str(Path(__file__).parent))
from test_run import _minimal_graph, _IsolatedDecisionsDir, refusing_chooser, run_mod, GRAPHS_DIR

class TransitionStatusTests(_IsolatedDecisionsDir):
    def test_selected_edges_are_recorded_without_inventing_alternate_paths(self):
        graph=_minimal_graph(states={'a':{'always':'b'},'b':{'type':'final'},'unused':{'type':'final'}})
        started=run_mod.start(graph,{},graphs_dir=GRAPHS_DIR,chooser=refusing_chooser,ckpt_id=None)
        status=run_mod.status(started['run'])
        self.assertEqual(status['outcome'],'reached')
        self.assertEqual([(t['source'],t['target']) for t in status['transitions']],[('a','b')])
        status['transitions'].clear()
        self.assertEqual(len(run_mod.status(started['run'])['transitions']),1)

    def test_tool_evidence_remains_after_a_fast_failure(self):
        graph=_minimal_graph(states={'a':{'entry':[{'type':'tool','params':{'name':'bash','input':{'command':'probe'}}}], 'always':'b'},'b':{'type':'final'}})
        started=run_mod.start(graph,{},graphs_dir=GRAPHS_DIR,chooser=refusing_chooser,ckpt_id=None)
        run_mod.step(started['run'],started['request']['id'],{'error':'probe refused','text':'denied'})
        status=run_mod.status(started['run'])
        self.assertEqual(status['outcome'],'error')
        self.assertEqual([e['kind'] for e in status['trace']],['node_enter','tool_call','tool_result'])
        self.assertEqual(status['trace'][1]['input'],{'command':'probe'})
        self.assertEqual(status['trace'][2]['result']['error'],'probe refused')
        status['trace'][2]['result'].clear()
        self.assertEqual(run_mod.status(started['run'])['trace'][2]['result']['text'],'denied')
