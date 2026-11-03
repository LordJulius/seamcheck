package org.x.examples;

import org.x.select.Traversor;
import org.x.select.Visitor;

public class Printer implements Visitor {
    public void head(int n) {
    }

    public void run() {
        Traversor.walk(this);
    }
}
