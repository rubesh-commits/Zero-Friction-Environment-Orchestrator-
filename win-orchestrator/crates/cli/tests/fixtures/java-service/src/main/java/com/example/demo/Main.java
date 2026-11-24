package com.example.demo;

import java.util.List;

/** Acceptance fixture: records, var and collection factories need Java 16+. */
record Greeting(String message) {}

public final class Main {
    public static void main(String[] args) {
        var names = List.of("acceptance", "windows");
        var greeting = new Greeting("java-service says hello to " + String.join(", ", names));
        System.out.println(greeting.message());
    }
}
