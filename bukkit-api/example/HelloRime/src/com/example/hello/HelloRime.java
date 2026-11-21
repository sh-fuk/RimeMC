package com.example.hello;

import org.bukkit.Bukkit;
import org.bukkit.ChatColor;
import org.bukkit.GameMode;
import org.bukkit.Location;
import org.bukkit.Material;
import org.bukkit.command.Command;
import org.bukkit.command.CommandSender;
import org.bukkit.entity.Player;
import org.bukkit.event.EventHandler;
import org.bukkit.event.EventPriority;
import org.bukkit.event.Listener;
import org.bukkit.event.block.BlockBreakEvent;
import org.bukkit.event.block.BlockPlaceEvent;
import org.bukkit.event.player.AsyncPlayerChatEvent;
import org.bukkit.event.player.PlayerJoinEvent;
import org.bukkit.event.player.PlayerQuitEvent;
import org.bukkit.inventory.ItemStack;
import org.bukkit.plugin.java.JavaPlugin;
import org.bukkit.scheduler.BukkitRunnable;

public class HelloRime extends JavaPlugin implements Listener {
    private int ticks;

    @Override public void onEnable() {
        saveDefaultConfig();
        getServer().getPluginManager().registerEvents(this, this);
        getCommand("hello").setExecutor(this);
        new BukkitRunnable() { @Override public void run() { ticks++; } }.runTaskTimer(this, 1, 1);
        Bukkit.getScheduler().runTaskLater(this, () -> getLogger().info("one second after enable, ticks=" + ticks), 20);
        getLogger().info("HelloRime enabled on " + Bukkit.getVersion() + ", protect=" + getConfig().getString("protect"));
    }

    @Override public void onDisable() { getLogger().info("HelloRime disabled"); }

    @EventHandler public void onJoin(PlayerJoinEvent e) {
        e.setJoinMessage(ChatColor.GOLD + "+ " + e.getPlayer().getName());
        String g = getConfig().getString("greeting", "hi").replace("%name%", e.getPlayer().getName());
        e.getPlayer().sendMessage(ChatColor.translateAlternateColorCodes('&', g));
    }

    @EventHandler public void onQuit(PlayerQuitEvent e) { e.setQuitMessage(ChatColor.GRAY + "- " + e.getPlayer().getName()); }

    @EventHandler(priority = EventPriority.HIGH, ignoreCancelled = true)
    public void onChat(AsyncPlayerChatEvent e) {
        for (String w : getConfig().getStringList("blocked-words"))
            if (e.getMessage().toLowerCase().contains(w)) { e.setCancelled(true); e.getPlayer().sendMessage(ChatColor.RED + "Watch your language!"); return; }
        e.setFormat(ChatColor.AQUA + "[%1$s]" + ChatColor.RESET + " %2$s");
    }

    @EventHandler public void onBreak(BlockBreakEvent e) {
        Material m = Material.matchMaterial(getConfig().getString("protect"));
        if (e.getBlock().getType() == m) { e.setCancelled(true); e.getPlayer().sendMessage(ChatColor.RED + "That block is protected"); }
    }

    @EventHandler public void onPlace(BlockPlaceEvent e) {
        if (e.getPlacedType() == Material.TNT) { e.setCancelled(true); e.getPlayer().sendMessage(ChatColor.RED + "No TNT here"); }
    }

    @Override public boolean onCommand(CommandSender sender, Command cmd, String label, String[] args) {
        if (cmd.getName().equals("secret")) { sender.sendMessage("the secret is 42"); return true; }
        if (!(sender instanceof Player)) { sender.sendMessage("players only"); return true; }
        Player p = (Player) sender;
        if (args.length == 0) { p.sendMessage("Hello, " + p.getName() + "! You are in " + p.getGameMode() + " at " + p.getLocation().getBlockY()); return true; }
        switch (args[0]) {
            case "give": p.getInventory().addItem(new ItemStack(Material.DIAMOND, 3)); p.sendMessage("gave 3 diamonds"); return true;
            case "tp": { Location l = p.getLocation(); l.add(0, 10, 0); p.teleport(l); p.sendMessage("up you go"); return true; }
            case "time": p.getWorld().setTime(13000); p.sendMessage("time=" + p.getWorld().getTime()); return true;
            case "ticks": p.sendMessage("ticks=" + ticks); return true;
            case "block": { Location l = p.getLocation(); l.add(2, 0, 0); l.getBlock().setType(Material.GOLD_BLOCK); p.sendMessage("placed gold at " + l.getBlockX() + " " + l.getBlockY() + " " + l.getBlockZ() + " -> " + l.getBlock().getType()); return true; }
            case "creative": p.setGameMode(GameMode.CREATIVE); return true;
            default: return false;
        }
    }
}
