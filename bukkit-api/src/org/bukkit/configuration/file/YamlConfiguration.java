package org.bukkit.configuration.file;

import java.io.File;
import java.io.Reader;

public class YamlConfiguration extends FileConfiguration {
    public YamlConfiguration() {}
    @Override public String saveToString() { return dev.rime.bukkit.Yaml.dump(toMap()); }
    @Override public void loadFromString(String contents) { map.clear(); load(dev.rime.bukkit.Yaml.parse(contents)); }
    public static YamlConfiguration loadConfiguration(File file) {
        YamlConfiguration c = new YamlConfiguration();
        try { if (file.exists()) c.load(file); } catch (Exception e) { dev.rime.bukkit.Bridge.log(1, "Cannot load " + file + ": " + e); }
        return c;
    }
    public static YamlConfiguration loadConfiguration(Reader reader) {
        YamlConfiguration c = new YamlConfiguration();
        try { c.load(reader); } catch (Exception e) { dev.rime.bukkit.Bridge.log(1, "Cannot load configuration: " + e); }
        return c;
    }
}
