# Design Philosophy

How to think about this system. If you internalize these principles, you should be able to predict why any component is structured the way it is — and make decisions that are consistent with the rest of the design without having to ask.

---

## Two root principles

Everything in this system flows from two ideas:

1. **Understand the true shape of the problem.**
2. **Design boundaries that match that shape.**

The first is about seeing clearly. The second is about acting on what you see. Every specific design rule in this project is a consequence of one or both.

---

## Understand the true shape

### Fix root causes, never paper over symptoms

If something is broken, find out why. A workaround that makes the symptom go away without understanding the cause is technical debt that compounds silently. This applies at every level: a rendering bug is not fixed by adjusting pixel offsets until it looks right — it's fixed by finding the incorrect assumption in the pipeline.

Defense-in-depth (assertions, validation, retry loops) is fine as a safety net if worth the added complexity. But it doesn't close the investigation.

### React to reality, don't poll for it

Event-driven over polling. If you need to know when something changes, set up a mechanism so it tells you — don't repeatedly ask. Polling is a workaround for not having a notification path. It's papering over a missing interface.

This is a specific case of fixing root causes: the root problem is "I need to react to state changes," and polling treats the symptom instead of solving the problem.

### Validate assumptions at the highest leverage point first

The cost of a wrong assumption is proportional to how many decisions sit on top of it. Before building, identify which assumption, if wrong, invalidates the most work — and test that one first. Settle the architecture before investing in the implementation. Spike before you build.

### When independent paths converge, trust the convergence

If you arrive at the same answer from two completely different directions — different starting assumptions, different reasoning chains, different prior art — that's stronger evidence than any single argument. They're all seeing the same underlying shape.

---

## Design boundaries that match the shape

### The system is a series of data transformations

Every part of the system, at every level of abstraction, has the same structure: data of one shape goes in, data of another shape goes out, and the translation logic is fully encapsulated. A component is a black box defined entirely by its input and output shapes.

This model is fractal. Zoom into any black box and it's itself a pipeline of smaller transformations. Zoom out and any subsystem collapses to a single translator. At the highest level: `user intent → [OS] → perceptual feedback`.

### The architecture is the interfaces, not the components

Components come and go. Implementations get rewritten. The interfaces — the data shapes between components — are what make the system a system. Design the interfaces first. The components follow. Settle the approach before choosing the technology.

When adding something new, the question is never "which component should this go in?" It's "which data transformation is this?"

### Push complexity outward to the leaves

Total _essential_ complexity is conserved — it can only be moved, not eliminated. Put it in leaf nodes: the outermost components that don't connect to anything downstream. The font rasterizer, the device driver, the format parser. Complex inside, simple interface. The complexity is contained — it can't leak.

Keep the connective tissue simple: the interfaces, protocols, and relationships between components. If the connective tissue is complex, the boundaries are in the wrong place. A messy interface is never solved by adding more surface area. It's solved by moving the boundary.

This is the same principle as functional programming's "pure core, side effects at the boundaries." The pure core is the simple connective tissue. The side effects are the leaf-node complexity. Both approaches achieve the same thing: a system you can reason about from the center outward, where surprises are confined to the edges.

The practical payoff: leaf nodes can be rewritten, optimized, or replaced without affecting anything else. The rendering backend can switch from CPU scanline rasterization to GPU compute shaders. A font library can be swapped out. A driver can be replaced for different hardware. None of these changes ripple inward, because the interface absorbs the variation. The system is as maintainable and extensible as its interfaces are stable.

### Isolate uncertain decisions behind interfaces

When you must act before a question is fully settled, put an interface in front of it. Code against the interface, never the implementation. The cost of the abstraction is small. The cost of a rewrite when the decision changes is large.

This is "push complexity to the leaves" applied to time: the uncertain decision IS a leaf node. You'll swap it out when you learn more. The interface ensures that swap is cheap.

### Find the abstraction that absorbs the edge cases

Real simplicity isn't avoiding hard cases — it's finding the abstraction where they stop being special. The test: when a new use case appears that you didn't explicitly design for, does the abstraction handle it naturally? If yes, the boundary is right. If you need a special case, the abstraction is wrong.

When you genuinely can't absorb an edge case, that's a pressure point. Document it. Don't warp the abstraction to accommodate it.
