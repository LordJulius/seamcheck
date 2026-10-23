import com.tngtech.archunit.core.domain.*;
import com.tngtech.archunit.core.importer.ClassFileImporter;
import com.tngtech.archunit.lang.EvaluationResult;
import java.util.*;
import static com.tngtech.archunit.lang.syntax.ArchRuleDefinition.noClasses;

// For every pair of packages under a root: does ArchUnit's
// noClasses().that().resideInAPackage(A..).should().dependOnClassesThat().resideInAPackage(B..) fail?
public class PkgDeps {
    public static void main(String[] args) {
        long t0 = System.currentTimeMillis();
        JavaClasses classes = new ClassFileImporter().importPath(args[0]);
        String root = args[1];
        Set<String> pkgs = new TreeSet<>();
        for (JavaClass c : classes) {
            String p = c.getPackageName();
            if (p.startsWith(root + ".")) pkgs.add(p.substring(root.length() + 1).split("\\.")[0]);
        }
        int rules = 0;
        for (String a : pkgs) for (String b : pkgs) {
            if (a.equals(b)) continue;
            rules++;
            EvaluationResult r = noClasses().that().resideInAPackage(root + "." + a + "..")
                .should().dependOnClassesThat().resideInAPackage(root + "." + b + "..").allowEmptyShould(true).evaluate(classes);
            if (r.hasViolation()) System.out.println(a + " -> " + b);
        }
        System.out.println("# rules=" + rules + " ms=" + (System.currentTimeMillis() - t0));
    }
}
