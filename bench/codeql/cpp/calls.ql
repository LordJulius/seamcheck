/** Every call CodeQL resolves to a function defined in the project. */

import cpp

from Call c, Function t
where
  t = c.getTarget() and
  t.hasDefinition() and
  exists(c.getFile().getRelativePath()) and
  exists(t.getDefinition().getFile().getRelativePath())
select c.getFile().getRelativePath(), c.getLocation().getStartLine(), t.getName(),
  t.getDefinition().getFile().getRelativePath(), t.getDefinition().getLocation().getStartLine()
