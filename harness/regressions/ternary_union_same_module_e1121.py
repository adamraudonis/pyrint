"""Same-module multi-branch ternary union: pylint STILL emits E1121.

This guards the gml.py:516 networkx false-positive fix from over-broadening.
The real networkx case (G = DiGraph()/Graph() and MultiDiGraph()/MultiGraph()
ternaries, then G.has_edge(u, v, key)) is suppressed by prylint because
astroid's deep CROSS-MODULE lazy inference yields an Uninferable among the
resolved BoundMethods, so pylint's safe_infer(node.func) returns None and
emits nothing. When the whole class hierarchy lives in ONE module (no
cross-module inference depth), astroid produces NO Uninferable and pylint
DOES emit E1121 -- so prylint must emit it here too.
"""


class Graph:
    def has_edge(self, u, v):
        return False


class DiGraph(Graph):
    pass


class MultiGraph(Graph):
    def has_edge(self, u, v, key=None):
        return False


class MultiDiGraph(MultiGraph):
    pass


def parse(directed, multigraph, edges):
    if not multigraph:
        graph = DiGraph() if directed else Graph()
    else:
        graph = MultiDiGraph() if directed else MultiGraph()
    for source, target, key in edges:
        if key is not None and graph.has_edge(source, target, key):
            raise ValueError("dup")
    return graph
