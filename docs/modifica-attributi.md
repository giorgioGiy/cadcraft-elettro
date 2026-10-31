# Modifica degli attributi di un blocco

Digitare `ATTRIBUTI` (oppure `ATTEDIT` / `EATTEDIT`) e fare clic sul blocco.
La stessa funzione è disponibile nella barra superiore, con il pulsante
“Modifica attributi del blocco”, e nel menu Modify > Object > Attribute > Single.

La finestra presenta due colonne: tipo di attributo (tag) e valore. I valori
possono essere vuoti o contenere più righe. Gli attributi invisibili si mostrano
con **Mostra attributi nascosti**; quelli costanti sono di sola lettura.
Nei blocchi con serie ripetute di attributi, il selettore separa **Cartiglio /
dati comuni** e **Colonna 1, 2, …**. Le colonne con una denominazione la mostrano
nel selettore. Si modifica una serie alla volta; passando a un altro gruppo,
i valori digitati restano in bozza fino a Salva o Annulla. Attributi con lo
stesso nome restano distinti e sono salvati nella loro posizione originale.
La finestra adatta la larghezza ai contenuti e allo schermo, con righe compatte.
La lista scorre per i blocchi con molti attributi; intestazioni e pulsanti
rimangono visibili durante lo scorrimento.

- **Salva** applica tutti i valori e chiude la finestra. Una sola operazione
  Annulla/Undo ripristina i valori precedenti. Salvare poi il disegno per
  aggiornare il file su disco.
- **Annulla** o **Escape** chiude senza applicare i valori digitati.

Il comando modifica solo il riferimento scelto, non tutte le copie dello
stesso blocco. Posizione, geometria e proprietà degli attributi rimangono
invariate. Un blocco senza attributi modificabili lascia attiva la richiesta
di selezione, con un messaggio.

Le chiamate JSON `attedit {"handle":"...","values":{"TAG":"valore"}}`
restano prive di finestre; la CLI senza interfaccia mantiene le domande testuali.

## Revisioni

- 2026-10-24: gruppi per colonne ripetute e filtro degli attributi nascosti.
- 2026-10-22: protezione dell'editor contro errori interni: annulla i valori
  non confermati e registra l'errore senza propagarlo alla finestra principale.
- 2026-10-22: dimensioni adattive, righe compatte e intestazioni fisse.
- 2026-10-22: editor a due colonne integrato con ATTEDIT.
