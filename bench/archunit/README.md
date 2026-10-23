ArchUnit side of the architecture comparison in the main README.

    javac -cp archunit-1.3.0.jar:slf4j-api-2.0.13.jar PkgDeps.java Cycles.java
    java -cp archunit-1.3.0.jar:slf4j-api-2.0.13.jar:. PkgDeps CLASSES_DIR org.jsoup
    java -cp archunit-1.3.0.jar:slf4j-api-2.0.13.jar:. Cycles CLASSES_DIR 'org.jsoup.(**)'

PkgDeps prints each `A -> B` whose rule `noClasses().that().resideInAPackage(root.A..)
.should().dependOnClassesThat().resideInAPackage(root.B..)` fails; compare with
`seamcheck graph 'src/main/java/org/jsoup/**' --level dir --via any --format json`.
