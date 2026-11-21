package org.bukkit.configuration.file;

import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;

import org.bukkit.configuration.MemoryConfiguration;

public abstract class FileConfiguration extends MemoryConfiguration {
    public FileConfiguration() {}
    public abstract String saveToString();
    public abstract void loadFromString(String contents);
    public void save(File file) throws IOException {
        if (file.getParentFile() != null) file.getParentFile().mkdirs();
        Files.write(file.toPath(), saveToString().getBytes(StandardCharsets.UTF_8));
    }
    public void save(String file) throws IOException { save(new File(file)); }
    public void load(File file) throws IOException { loadFromString(new String(Files.readAllBytes(file.toPath()), StandardCharsets.UTF_8)); }
    public void load(Reader reader) throws IOException {
        StringBuilder b = new StringBuilder();
        char[] buf = new char[4096];
        int n;
        while ((n = reader.read(buf)) > 0) b.append(buf, 0, n);
        loadFromString(b.toString());
    }
}
