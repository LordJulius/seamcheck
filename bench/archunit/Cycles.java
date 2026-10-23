import com.tngtech.archunit.core.domain.JavaClasses;
import com.tngtech.archunit.core.importer.ClassFileImporter;
import com.tngtech.archunit.lang.EvaluationResult;
import java.util.*;
import java.util.regex.*;
import static com.tngtech.archunit.library.dependencies.SlicesRuleDefinition.slices;

public class Cycles {
    public static void main(String[] args) {
        long t0 = System.currentTimeMillis();
        JavaClasses classes = new ClassFileImporter().importPath(args[0]);
        EvaluationResult r = slices().matching(args[1]).should().beFreeOfCycles().evaluate(classes);
        Set<String> inCycles = new TreeSet<>();
        Set<String> edges = new TreeSet<>();
        Pattern p = Pattern.compile("Slice ([\\w.$]+)");
        for (String d : r.getFailureReport().getDetails()) {
            if (!d.startsWith("Cycle detected")) continue;
            String head = d.split("\n\n")[0];
            Matcher m = p.matcher(head);
            List<String> names = new ArrayList<>();
            while (m.find()) names.add(m.group(1));
            inCycles.addAll(names);
            for (int i = 0; i + 1 < names.size(); i++) edges.add(names.get(i) + " -> " + names.get(i + 1));
        }
        System.out.println("cycles=" + r.getFailureReport().getDetails().size() + " packages=" + inCycles);
        System.out.println("classes=" + classes.size() + " ms=" + (System.currentTimeMillis() - t0));
    }
}
