# Navigazione dei fogli elettrici

Per organizzare più fogli e modificarne i cartigli usare anche il comando
`PAGINE`: vedere [Gestione pagine](gestione-pagine.md).

I comandi `sheet.previous` (`FP`) e `sheet.next` (`AV`) cambiano foglio nei
disegni organizzati con layer chiamati `1`, `2`, `3`, ecc. Sono disponibili
anche nella barra degli strumenti e nel menu View > Fogli.

Il foglio richiesto diventa corrente, acceso e scongelato. Gli altri layer
numerici vengono congelati. Tutti i layer comuni conservano il loro stato,
compresi Cartiglio e i layer tecnici dei blocchi. Geometria e attributi non
vengono modificati. Il cambio può essere annullato con Undo.

Se il layer corrente non è un foglio numerico, entrambi i comandi cercano il
foglio 1. Un foglio mancante produce un messaggio senza modificare il disegno:
non si saltano numeri mancanti e non si ricomincia automaticamente dal primo.
Il layer `0` e nomi come `01` non sono fogli.

Questa funzione riprende la navigazione AV/FP fornita in Lisp. Non esegue la
conversione TAVOLEZERO e non cambia automaticamente lo stato dei layer tecnici
come TZFOGLIO.

## Revisioni

- 2026-10-22: prima implementazione dei comandi e dei pulsanti di navigazione.
