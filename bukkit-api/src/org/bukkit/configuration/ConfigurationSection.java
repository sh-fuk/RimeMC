package org.bukkit.configuration;

import java.util.List;
import java.util.Map;
import java.util.Set;

public interface ConfigurationSection {
    Set<String> getKeys(boolean deep);
    Map<String, Object> getValues(boolean deep);
    boolean contains(String path);
    boolean isSet(String path);
    Object get(String path);
    Object get(String path, Object def);
    void set(String path, Object value);
    String getString(String path);
    String getString(String path, String def);
    int getInt(String path);
    int getInt(String path, int def);
    long getLong(String path);
    long getLong(String path, long def);
    double getDouble(String path);
    double getDouble(String path, double def);
    boolean getBoolean(String path);
    boolean getBoolean(String path, boolean def);
    List<?> getList(String path);
    List<String> getStringList(String path);
    List<Integer> getIntegerList(String path);
    ConfigurationSection getConfigurationSection(String path);
    ConfigurationSection createSection(String path);
    boolean isConfigurationSection(String path);
    String getName();
    String getCurrentPath();
}
